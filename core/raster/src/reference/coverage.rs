use pdf_rs_scene::FillRule;

use super::geometry::{
    Fixed, FixedPoint, FlattenedPath, GeometryFailure, GeometryLimitKind, GeometryWork,
    logical_vector_capacity,
};

pub(crate) const SAMPLE_GRID_WIDTH: u32 = 8;
pub(crate) const SAMPLES_PER_PIXEL: u32 = SAMPLE_GRID_WIDTH * SAMPLE_GRID_WIDTH;
pub(crate) const FULL_SAMPLE_MASK: u64 = u64::MAX;
const MASK_INITIALIZATION_CHUNK_PIXELS: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Edge {
    start: FixedPoint,
    end: FixedPoint,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FillEdges {
    edges: Vec<Edge>,
    bounds: Option<FixedBounds>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FixedBounds {
    minimum: FixedPoint,
    maximum: FixedPoint,
}

impl FillEdges {
    pub(crate) fn from_path(
        path: &FlattenedPath,
        work: &mut GeometryWork<'_>,
    ) -> Result<Self, GeometryFailure> {
        let mut edges = Vec::new();
        let mut bounds: Option<FixedBounds> = None;
        for subpath in path.subpaths() {
            work.charge_fuel(1)?;
            let points = subpath.points();
            if points.len() < 2 {
                continue;
            }
            for pair in points.windows(2) {
                push_fill_edge(&mut edges, &mut bounds, pair[0], pair[1], work)?;
            }
            push_fill_edge(
                &mut edges,
                &mut bounds,
                *points.last().ok_or(GeometryFailure::InvalidGeometry)?,
                points[0],
                work,
            )?;
        }
        Ok(Self { edges, bounds })
    }

    pub(crate) fn contains(
        &self,
        point: FixedPoint,
        rule: FillRule,
        work: &mut GeometryWork<'_>,
    ) -> Result<bool, GeometryFailure> {
        let mut winding = 0_i64;
        let mut parity = false;
        for edge in &self.edges {
            work.charge_fuel(1)?;
            let start_y = edge.start.y.raw();
            let end_y = edge.end.y.raw();
            let upward = start_y <= point.y.raw() && point.y.raw() < end_y;
            let downward = end_y <= point.y.raw() && point.y.raw() < start_y;
            if !upward && !downward {
                continue;
            }
            let edge_x = i128::from(edge.end.x.raw())
                .checked_sub(i128::from(edge.start.x.raw()))
                .ok_or(GeometryFailure::NumericOverflow)?;
            let edge_y = i128::from(edge.end.y.raw())
                .checked_sub(i128::from(edge.start.y.raw()))
                .ok_or(GeometryFailure::NumericOverflow)?;
            let sample_x = i128::from(point.x.raw())
                .checked_sub(i128::from(edge.start.x.raw()))
                .ok_or(GeometryFailure::NumericOverflow)?;
            let sample_y = i128::from(point.y.raw())
                .checked_sub(i128::from(edge.start.y.raw()))
                .ok_or(GeometryFailure::NumericOverflow)?;
            let cross = edge_x
                .checked_mul(sample_y)
                .and_then(|left| {
                    edge_y
                        .checked_mul(sample_x)
                        .and_then(|right| left.checked_sub(right))
                })
                .ok_or(GeometryFailure::NumericOverflow)?;
            let crosses_right_ray = (upward && cross > 0) || (downward && cross < 0);
            if crosses_right_ray {
                match rule {
                    FillRule::EvenOdd => parity = !parity,
                    FillRule::Nonzero if upward => {
                        winding = winding
                            .checked_add(1)
                            .ok_or(GeometryFailure::NumericOverflow)?;
                    }
                    FillRule::Nonzero => {
                        winding = winding
                            .checked_sub(1)
                            .ok_or(GeometryFailure::NumericOverflow)?;
                    }
                }
            }
        }
        Ok(match rule {
            FillRule::Nonzero => winding != 0,
            FillRule::EvenOdd => parity,
        })
    }

    fn comparison_bound(&self) -> Result<u64, GeometryFailure> {
        u64::try_from(self.edges.len()).map_err(|_| GeometryFailure::NumericOverflow)
    }

    fn pixel_bounds(&self, width: u32, height: u32) -> Result<PixelBounds, GeometryFailure> {
        let Some(bounds) = self.bounds else {
            return Ok(PixelBounds::EMPTY);
        };
        let start_x = floor_fixed(bounds.minimum.x)?;
        let start_y = floor_fixed(bounds.minimum.y)?;
        let end_x = ceil_fixed(bounds.maximum.x)?;
        let end_y = ceil_fixed(bounds.maximum.y)?;
        Ok(PixelBounds {
            start_x: clamp_coordinate(start_x, width),
            start_y: clamp_coordinate(start_y, height),
            end_x: clamp_coordinate(end_x, width),
            end_y: clamp_coordinate(end_y, height),
        })
    }
}

fn push_fill_edge(
    edges: &mut Vec<Edge>,
    bounds: &mut Option<FixedBounds>,
    start: FixedPoint,
    end: FixedPoint,
    work: &mut GeometryWork<'_>,
) -> Result<(), GeometryFailure> {
    work.charge_edges(1)?;
    if start == end || start.y == end.y {
        return Ok(());
    }
    work.try_push_geometry(edges, Edge { start, end })?;
    include_point(bounds, start);
    include_point(bounds, end);
    Ok(())
}

fn include_point(bounds: &mut Option<FixedBounds>, point: FixedPoint) {
    match bounds {
        Some(bounds) => {
            bounds.minimum.x = bounds.minimum.x.min(point.x);
            bounds.minimum.y = bounds.minimum.y.min(point.y);
            bounds.maximum.x = bounds.maximum.x.max(point.x);
            bounds.maximum.y = bounds.maximum.y.max(point.y);
        }
        None => {
            *bounds = Some(FixedBounds {
                minimum: point,
                maximum: point,
            });
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PixelBounds {
    start_x: u32,
    start_y: u32,
    end_x: u32,
    end_y: u32,
}

impl PixelBounds {
    const EMPTY: Self = Self {
        start_x: 0,
        start_y: 0,
        end_x: 0,
        end_y: 0,
    };
}

fn floor_fixed(value: Fixed) -> Result<i64, GeometryFailure> {
    let one = Fixed::ONE.raw();
    let quotient = value.raw() / one;
    let remainder = value.raw() % one;
    if remainder < 0 {
        quotient
            .checked_sub(1)
            .ok_or(GeometryFailure::NumericOverflow)
    } else {
        Ok(quotient)
    }
}

fn ceil_fixed(value: Fixed) -> Result<i64, GeometryFailure> {
    let floor = floor_fixed(value)?;
    if value.raw() % Fixed::ONE.raw() == 0 {
        Ok(floor)
    } else {
        floor.checked_add(1).ok_or(GeometryFailure::NumericOverflow)
    }
}

fn clamp_coordinate(value: i64, maximum: u32) -> u32 {
    if value <= 0 {
        0
    } else {
        u32::try_from(value).unwrap_or(u32::MAX).min(maximum)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CoverageMask {
    width: u32,
    height: u32,
    samples: Vec<u64>,
}

impl CoverageMask {
    pub(crate) fn empty(
        width: u32,
        height: u32,
        work: &mut GeometryWork<'_>,
    ) -> Result<Self, GeometryFailure> {
        let pixel_count = Self::initialization_fuel(width, height)?;
        let semantic_bytes = pixel_count
            .checked_mul(8)
            .ok_or(GeometryFailure::NumericOverflow)?;
        ensure_coverage_bytes(0, semantic_bytes, work)?;
        work.preflight_fuel(pixel_count)?;
        work.check_cancellation()?;
        let pixel_count =
            usize::try_from(pixel_count).map_err(|_| GeometryFailure::NumericOverflow)?;
        let mut samples = Vec::new();
        samples
            .try_reserve_exact(pixel_count)
            .map_err(|_| GeometryFailure::Allocation {
                attempted_bytes: semantic_bytes,
            })?;
        let retained_bytes = capacity_bytes::<u64>(samples.capacity())?;
        work.observe_coverage_bytes(retained_bytes);
        work.note_working_bytes(retained_bytes)?;
        ensure_coverage_bytes(0, retained_bytes, work)?;
        while samples.len() < pixel_count {
            let next = samples
                .len()
                .checked_add(MASK_INITIALIZATION_CHUNK_PIXELS)
                .ok_or(GeometryFailure::NumericOverflow)?
                .min(pixel_count);
            let chunk = next
                .checked_sub(samples.len())
                .ok_or(GeometryFailure::NumericOverflow)?;
            work.charge_fuel(u64::try_from(chunk).map_err(|_| GeometryFailure::NumericOverflow)?)?;
            samples.resize(next, 0);
        }
        Ok(Self {
            width,
            height,
            samples,
        })
    }

    #[allow(
        dead_code,
        reason = "mounted resource kernels validate clip dimensions"
    )]
    pub(crate) const fn width(&self) -> u32 {
        self.width
    }

    #[allow(
        dead_code,
        reason = "mounted resource kernels validate clip dimensions"
    )]
    pub(crate) const fn height(&self) -> u32 {
        self.height
    }

    #[allow(
        dead_code,
        reason = "the analytic integration harness inspects exact masks"
    )]
    pub(crate) fn samples(&self) -> &[u64] {
        &self.samples
    }

    pub(crate) fn retained_bytes(&self) -> Result<u64, GeometryFailure> {
        capacity_bytes::<u64>(self.samples.capacity())
    }

    pub(crate) fn initialization_fuel(width: u32, height: u32) -> Result<u64, GeometryFailure> {
        u64::from(width)
            .checked_mul(u64::from(height))
            .ok_or(GeometryFailure::NumericOverflow)
    }

    pub(crate) fn sample_mask(&self, x: u32, y: u32) -> Option<u64> {
        self.index(x, y).map(|index| self.samples[index])
    }

    #[allow(
        dead_code,
        reason = "the analytic integration harness inspects scalar coverage"
    )]
    pub(crate) fn coverage(&self, x: u32, y: u32) -> Option<u8> {
        self.sample_mask(x, y)
            .map(|mask| u8::try_from(mask.count_ones()).expect("8x8 coverage fits u8"))
    }

    pub(crate) fn set_sample_mask(
        &mut self,
        x: u32,
        y: u32,
        samples: u64,
    ) -> Result<(), GeometryFailure> {
        let index = self.index(x, y).ok_or(GeometryFailure::InvalidGeometry)?;
        self.samples[index] = samples;
        Ok(())
    }

    fn index(&self, x: u32, y: u32) -> Option<usize> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let index = u64::from(y)
            .checked_mul(u64::from(self.width))?
            .checked_add(u64::from(x))?;
        usize::try_from(index).ok()
    }
}

pub(crate) fn rasterize_fill(
    edges: &FillEdges,
    rule: FillRule,
    width: u32,
    height: u32,
    work: &mut GeometryWork<'_>,
) -> Result<CoverageMask, GeometryFailure> {
    let requirements = fill_requirements(edges, width, height, work)?;
    let initialization_fuel = CoverageMask::initialization_fuel(width, height)?;
    let total_fuel = initialization_fuel
        .checked_add(requirements.raster_fuel)
        .ok_or(GeometryFailure::NumericOverflow)?;
    work.preflight_fuel(total_fuel)?;
    let mut mask = CoverageMask::empty(width, height, work)?;
    rasterize_fill_pixels(edges, rule, requirements.bounds, &mut mask, false, work)?;
    Ok(mask)
}

/// Rasterizes another fill into an existing mask using sample-wise union.
///
/// This preserves the canonical 8x8 sample positions while allowing one paint operation, such as
/// a glyph run, to merge multiple independently transformed outlines without allocating a full
/// temporary mask for every outline.
#[allow(
    dead_code,
    reason = "the staged geometry-only harness compiles coverage without the staged glyph adapter"
)]
pub(crate) fn rasterize_fill_union(
    edges: &FillEdges,
    rule: FillRule,
    mask: &mut CoverageMask,
    work: &mut GeometryWork<'_>,
) -> Result<(), GeometryFailure> {
    let requirements = fill_requirements(edges, mask.width, mask.height, work)?;
    work.preflight_fuel(requirements.raster_fuel)?;
    rasterize_fill_pixels(edges, rule, requirements.bounds, mask, true, work)
}

#[derive(Clone, Copy)]
struct FillRequirements {
    bounds: PixelBounds,
    raster_fuel: u64,
}

fn fill_requirements(
    edges: &FillEdges,
    width: u32,
    height: u32,
    work: &GeometryWork<'_>,
) -> Result<FillRequirements, GeometryFailure> {
    let bounds = edges.pixel_bounds(width, height)?;
    let bounded_pixels = u64::from(bounds.end_x.saturating_sub(bounds.start_x))
        .checked_mul(u64::from(bounds.end_y.saturating_sub(bounds.start_y)))
        .ok_or(GeometryFailure::NumericOverflow)?;
    let samples = bounded_pixels
        .checked_mul(u64::from(SAMPLES_PER_PIXEL))
        .ok_or(GeometryFailure::NumericOverflow)?;
    work.preflight_samples(samples)?;
    let comparison_fuel = samples
        .checked_mul(edges.comparison_bound()?)
        .ok_or(GeometryFailure::NumericOverflow)?;
    let raster_fuel = samples
        .checked_add(comparison_fuel)
        .ok_or(GeometryFailure::NumericOverflow)?;
    Ok(FillRequirements {
        bounds,
        raster_fuel,
    })
}

fn rasterize_fill_pixels(
    edges: &FillEdges,
    rule: FillRule,
    bounds: PixelBounds,
    mask: &mut CoverageMask,
    union: bool,
    work: &mut GeometryWork<'_>,
) -> Result<(), GeometryFailure> {
    for y in bounds.start_y..bounds.end_y {
        for x in bounds.start_x..bounds.end_x {
            work.charge_samples(u64::from(SAMPLES_PER_PIXEL))?;
            let mut pixel_mask = 0_u64;
            for sample_y in 0..SAMPLE_GRID_WIDTH {
                for sample_x in 0..SAMPLE_GRID_WIDTH {
                    let point = sample_point(x, y, sample_x, sample_y)?;
                    if edges.contains(point, rule, work)? {
                        let bit = sample_y
                            .checked_mul(SAMPLE_GRID_WIDTH)
                            .and_then(|value| value.checked_add(sample_x))
                            .ok_or(GeometryFailure::NumericOverflow)?;
                        pixel_mask |= 1_u64
                            .checked_shl(bit)
                            .ok_or(GeometryFailure::NumericOverflow)?;
                    }
                }
            }
            let index = mask.index(x, y).ok_or(GeometryFailure::InvalidGeometry)?;
            mask.samples[index] = if union {
                mask.samples[index] | pixel_mask
            } else {
                pixel_mask
            };
        }
    }
    Ok(())
}

fn ensure_coverage_bytes(
    consumed: u64,
    additional: u64,
    work: &GeometryWork<'_>,
) -> Result<(), GeometryFailure> {
    let attempted = consumed
        .checked_add(additional)
        .ok_or(GeometryFailure::NumericOverflow)?;
    if attempted > work.limits().max_coverage_bytes {
        return Err(GeometryFailure::Limit {
            kind: GeometryLimitKind::CoverageBytes,
            limit: work.limits().max_coverage_bytes,
            consumed,
            attempted: additional,
        });
    }
    work.ensure_working_bytes(attempted)?;
    Ok(())
}

pub(crate) fn sample_point(
    pixel_x: u32,
    pixel_y: u32,
    sample_x: u32,
    sample_y: u32,
) -> Result<FixedPoint, GeometryFailure> {
    if sample_x >= SAMPLE_GRID_WIDTH || sample_y >= SAMPLE_GRID_WIDTH {
        return Err(GeometryFailure::InvalidGeometry);
    }
    let pixel_x = Fixed::from_i64(i64::from(pixel_x))?;
    let pixel_y = Fixed::from_i64(i64::from(pixel_y))?;
    let offset_x = i64::from(
        sample_x
            .checked_mul(2)
            .and_then(|value| value.checked_add(1))
            .ok_or(GeometryFailure::NumericOverflow)?,
    )
    .checked_mul(Fixed::ONE.raw() / 16)
    .ok_or(GeometryFailure::NumericOverflow)?;
    let offset_y = i64::from(
        sample_y
            .checked_mul(2)
            .and_then(|value| value.checked_add(1))
            .ok_or(GeometryFailure::NumericOverflow)?,
    )
    .checked_mul(Fixed::ONE.raw() / 16)
    .ok_or(GeometryFailure::NumericOverflow)?;
    Ok(FixedPoint::new(
        pixel_x.checked_add(Fixed::from_raw(offset_x))?,
        pixel_y.checked_add(Fixed::from_raw(offset_y))?,
    ))
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ClipStack {
    width: u32,
    height: u32,
    current: Option<Vec<u64>>,
    saved: Vec<Option<Vec<u64>>>,
    retained_bytes: u64,
    peak_retained_bytes: u64,
    operation_peak_retained_bytes: u64,
}

impl ClipStack {
    pub(crate) fn new(width: u32, height: u32) -> Result<Self, GeometryFailure> {
        if width == 0 || height == 0 {
            return Err(GeometryFailure::InvalidGeometry);
        }
        Ok(Self {
            width,
            height,
            current: None,
            saved: Vec::new(),
            retained_bytes: 0,
            peak_retained_bytes: 0,
            operation_peak_retained_bytes: 0,
        })
    }

    #[allow(
        dead_code,
        reason = "mounted resource kernels validate clip dimensions"
    )]
    pub(crate) const fn width(&self) -> u32 {
        self.width
    }

    #[allow(
        dead_code,
        reason = "mounted resource kernels validate clip dimensions"
    )]
    pub(crate) const fn height(&self) -> u32 {
        self.height
    }

    #[allow(
        dead_code,
        reason = "mounted dispatch accounts for incoming clip replacement masks"
    )]
    pub(crate) const fn has_mask(&self) -> bool {
        self.current.is_some()
    }

    pub(crate) fn save(&mut self, work: &mut GeometryWork<'_>) -> Result<(), GeometryFailure> {
        self.save_with_outer_reserve(work, |replacement, target_capacity, target_bytes| {
            replacement.try_reserve_exact(target_capacity).map_err(|_| {
                GeometryFailure::Allocation {
                    attempted_bytes: target_bytes,
                }
            })
        })
    }

    fn save_with_outer_reserve(
        &mut self,
        work: &mut GeometryWork<'_>,
        reserve: impl FnOnce(&mut Vec<Option<Vec<u64>>>, usize, u64) -> Result<(), GeometryFailure>,
    ) -> Result<(), GeometryFailure> {
        self.begin_operation();
        let attempted_depth = u32::try_from(self.saved.len())
            .map_err(|_| GeometryFailure::NumericOverflow)?
            .checked_add(1)
            .ok_or(GeometryFailure::NumericOverflow)?;
        if attempted_depth > work.limits().max_clip_depth {
            return Err(GeometryFailure::Limit {
                kind: GeometryLimitKind::ClipDepth,
                limit: u64::from(work.limits().max_clip_depth),
                consumed: u64::try_from(self.saved.len())
                    .map_err(|_| GeometryFailure::NumericOverflow)?,
                attempted: 1,
            });
        }
        let retained_before = self.retained_bytes;
        let outer_bytes_before = capacity_bytes::<Option<Vec<u64>>>(self.saved.capacity())?;
        let copy_items = self.current.as_ref().map_or(0, Vec::len);
        let copy_semantic_bytes = u64::try_from(copy_items)
            .map_err(|_| GeometryFailure::NumericOverflow)?
            .checked_mul(8)
            .ok_or(GeometryFailure::NumericOverflow)?;
        let required_outer_items = self
            .saved
            .len()
            .checked_add(1)
            .ok_or(GeometryFailure::NumericOverflow)?;
        let current_outer_items = logical_vector_capacity(self.saved.len())?;
        let target_outer_items = logical_vector_capacity(required_outer_items)?;
        if self.saved.capacity() < current_outer_items {
            return Err(GeometryFailure::InvalidGeometry);
        }
        let needs_outer_growth = target_outer_items != current_outer_items;
        let outer_semantic_bytes = if needs_outer_growth {
            capacity_bytes::<Option<Vec<u64>>>(target_outer_items)?
        } else {
            0
        };
        let semantic_additional = copy_semantic_bytes
            .checked_add(outer_semantic_bytes)
            .ok_or(GeometryFailure::NumericOverflow)?;
        self.ensure_clip_bytes(retained_before, semantic_additional, work)?;
        work.ensure_working_bytes(semantic_additional)?;
        let semantic_attempted_total = retained_before
            .checked_add(semantic_additional)
            .ok_or(GeometryFailure::NumericOverflow)?;

        let moved_outer_items = if needs_outer_growth {
            self.saved.len()
        } else {
            0
        };
        let operation_fuel = u64::try_from(
            copy_items
                .checked_add(moved_outer_items)
                .and_then(|value| value.checked_add(1))
                .ok_or(GeometryFailure::NumericOverflow)?,
        )
        .map_err(|_| GeometryFailure::NumericOverflow)?;
        work.preflight_fuel(operation_fuel)?;
        work.check_cancellation()?;

        let saved = if let Some(mask) = &self.current {
            let mut copy = Vec::new();
            copy.try_reserve_exact(mask.len())
                .map_err(|_| GeometryFailure::Allocation {
                    attempted_bytes: copy_semantic_bytes,
                })?;
            let copy_bytes = capacity_bytes::<u64>(copy.capacity())?;
            let copy_transient = retained_before
                .checked_add(copy_bytes)
                .ok_or(GeometryFailure::NumericOverflow)?;
            self.operation_peak_retained_bytes =
                self.operation_peak_retained_bytes.max(copy_transient);
            self.peak_retained_bytes = self.peak_retained_bytes.max(copy_transient);
            work.note_working_bytes(copy_bytes)?;
            self.ensure_clip_bytes(retained_before, copy_bytes, work)?;
            work.ensure_working_bytes(copy_bytes)?;
            for chunk in mask.chunks(MASK_INITIALIZATION_CHUNK_PIXELS) {
                work.charge_fuel(
                    u64::try_from(chunk.len()).map_err(|_| GeometryFailure::NumericOverflow)?,
                )?;
                copy.extend_from_slice(chunk);
            }
            Some(copy)
        } else {
            None
        };
        let saved_bytes = saved
            .as_ref()
            .map_or(Ok(0), |mask| capacity_bytes::<u64>(mask.capacity()))?;
        self.ensure_clip_bytes(retained_before, saved_bytes, work)?;

        if needs_outer_growth {
            let pre_replacement_additional = saved_bytes
                .checked_add(outer_semantic_bytes)
                .ok_or(GeometryFailure::NumericOverflow)?;
            self.ensure_clip_bytes(retained_before, pre_replacement_additional, work)?;
            let mut replacement = Vec::new();
            reserve(
                &mut replacement,
                target_outer_items,
                semantic_attempted_total,
            )?;
            let replacement_bytes = capacity_bytes::<Option<Vec<u64>>>(replacement.capacity())?;
            let transient_additional = saved_bytes
                .checked_add(replacement_bytes)
                .ok_or(GeometryFailure::NumericOverflow)?;
            let transient_retained = retained_before
                .checked_add(transient_additional)
                .ok_or(GeometryFailure::NumericOverflow)?;
            self.observe_operation_peak(transient_retained);
            work.note_working_bytes(transient_additional)?;
            self.ensure_clip_bytes(retained_before, transient_additional, work)?;
            work.ensure_working_bytes(transient_additional)?;
            let committed_retained = transient_retained
                .checked_sub(outer_bytes_before)
                .ok_or(GeometryFailure::NumericOverflow)?;

            work.charge_fuel(
                u64::try_from(moved_outer_items)
                    .map_err(|_| GeometryFailure::NumericOverflow)?
                    .checked_add(1)
                    .ok_or(GeometryFailure::NumericOverflow)?,
            )?;
            replacement.append(&mut self.saved);
            replacement.push(saved);
            self.saved = replacement;
            self.retained_bytes = committed_retained;
            self.observe_operation_peak(committed_retained);
        } else {
            let committed_retained = retained_before
                .checked_add(saved_bytes)
                .ok_or(GeometryFailure::NumericOverflow)?;
            work.charge_fuel(1)?;
            self.saved.push(saved);
            self.retained_bytes = committed_retained;
            self.observe_operation_peak(committed_retained);
        }
        Ok(())
    }

    pub(crate) fn restore(&mut self, work: &mut GeometryWork<'_>) -> Result<(), GeometryFailure> {
        self.begin_operation();
        if self.saved.is_empty() {
            return Err(GeometryFailure::InvalidGeometry);
        }
        let current_bytes = self
            .current
            .as_ref()
            .map_or(Ok(0), |mask| capacity_bytes::<u64>(mask.capacity()))?;
        let retained_before = self.retained_bytes;
        let committed_retained = self
            .retained_bytes
            .checked_sub(current_bytes)
            .ok_or(GeometryFailure::NumericOverflow)?;
        work.preflight_fuel(1)?;
        work.charge_fuel(1)?;
        let saved = self.saved.pop().ok_or(GeometryFailure::InvalidGeometry)?;
        self.current = saved;
        self.retained_bytes = committed_retained;
        self.observe_operation_peak(retained_before.max(committed_retained));
        Ok(())
    }

    pub(crate) fn intersect(
        &mut self,
        mask: CoverageMask,
        work: &mut GeometryWork<'_>,
    ) -> Result<(), GeometryFailure> {
        self.intersect_with_reserve(mask, work, |replacement, target_capacity, target_bytes| {
            replacement.try_reserve_exact(target_capacity).map_err(|_| {
                GeometryFailure::Allocation {
                    attempted_bytes: target_bytes,
                }
            })
        })
    }

    fn intersect_with_reserve(
        &mut self,
        mask: CoverageMask,
        work: &mut GeometryWork<'_>,
        reserve: impl FnOnce(&mut Vec<u64>, usize, u64) -> Result<(), GeometryFailure>,
    ) -> Result<(), GeometryFailure> {
        self.begin_operation();
        if mask.width != self.width || mask.height != self.height {
            return Err(GeometryFailure::InvalidGeometry);
        }
        if let Some(current) = &self.current {
            if current.len() != mask.samples.len() {
                return Err(GeometryFailure::InvalidGeometry);
            }
            let pixels =
                u64::try_from(current.len()).map_err(|_| GeometryFailure::NumericOverflow)?;
            let semantic_bytes = pixels
                .checked_mul(8)
                .ok_or(GeometryFailure::NumericOverflow)?;
            let incoming_bytes = mask.retained_bytes()?;
            let retained_before = self.retained_bytes;
            let current_bytes = capacity_bytes::<u64>(current.capacity())?;
            self.ensure_clip_bytes(retained_before, semantic_bytes, work)?;
            work.ensure_working_bytes(
                incoming_bytes
                    .checked_add(semantic_bytes)
                    .ok_or(GeometryFailure::NumericOverflow)?,
            )?;
            work.preflight_fuel(pixels)?;
            work.check_cancellation()?;

            let mut replacement = Vec::new();
            reserve(&mut replacement, current.len(), semantic_bytes)?;
            let replacement_bytes = capacity_bytes::<u64>(replacement.capacity())?;
            let transient_retained = retained_before
                .checked_add(replacement_bytes)
                .ok_or(GeometryFailure::NumericOverflow)?;
            self.operation_peak_retained_bytes =
                self.operation_peak_retained_bytes.max(transient_retained);
            self.peak_retained_bytes = self.peak_retained_bytes.max(transient_retained);
            let actual_additional = incoming_bytes
                .checked_add(replacement_bytes)
                .ok_or(GeometryFailure::NumericOverflow)?;
            work.note_working_bytes(actual_additional)?;
            self.ensure_clip_bytes(retained_before, replacement_bytes, work)?;
            work.ensure_working_bytes(actual_additional)?;
            for (current_chunk, incoming_chunk) in current
                .chunks(MASK_INITIALIZATION_CHUNK_PIXELS)
                .zip(mask.samples.chunks(MASK_INITIALIZATION_CHUNK_PIXELS))
            {
                work.charge_fuel(
                    u64::try_from(current_chunk.len())
                        .map_err(|_| GeometryFailure::NumericOverflow)?,
                )?;
                replacement.extend(
                    current_chunk
                        .iter()
                        .zip(incoming_chunk)
                        .map(|(current, incoming)| *current & *incoming),
                );
            }
            let committed_retained = retained_before
                .checked_sub(current_bytes)
                .and_then(|value| value.checked_add(replacement_bytes))
                .ok_or(GeometryFailure::NumericOverflow)?;
            self.current = Some(replacement);
            self.retained_bytes = committed_retained;
            self.observe_operation_peak(committed_retained);
        } else {
            let consumed = self.retained_bytes;
            let bytes = capacity_bytes::<u64>(mask.samples.capacity())?;
            self.ensure_clip_bytes(consumed, bytes, work)?;
            let committed_retained = consumed
                .checked_add(bytes)
                .ok_or(GeometryFailure::NumericOverflow)?;
            work.preflight_fuel(1)?;
            work.charge_fuel(1)?;
            self.current = Some(mask.samples);
            self.retained_bytes = committed_retained;
            self.observe_operation_peak(committed_retained);
        }
        Ok(())
    }

    #[allow(
        dead_code,
        reason = "the analytic clip harness tests transactional mask application"
    )]
    pub(crate) fn apply(
        &mut self,
        mask: &mut CoverageMask,
        work: &mut GeometryWork<'_>,
    ) -> Result<(), GeometryFailure> {
        self.apply_with_reserve(mask, work, |replacement, target_capacity, target_bytes| {
            replacement.try_reserve_exact(target_capacity).map_err(|_| {
                GeometryFailure::Allocation {
                    attempted_bytes: target_bytes,
                }
            })
        })
    }

    fn apply_with_reserve(
        &mut self,
        mask: &mut CoverageMask,
        work: &mut GeometryWork<'_>,
        reserve: impl FnOnce(&mut Vec<u64>, usize, u64) -> Result<(), GeometryFailure>,
    ) -> Result<(), GeometryFailure> {
        self.begin_operation();
        if mask.width != self.width || mask.height != self.height {
            return Err(GeometryFailure::InvalidGeometry);
        }
        if let Some(current) = &self.current {
            if current.len() != mask.samples.len() {
                return Err(GeometryFailure::InvalidGeometry);
            }
            let pixels =
                u64::try_from(current.len()).map_err(|_| GeometryFailure::NumericOverflow)?;
            let semantic_bytes = pixels
                .checked_mul(8)
                .ok_or(GeometryFailure::NumericOverflow)?;
            let incoming_bytes = mask.retained_bytes()?;
            let retained_before = self.retained_bytes;
            self.ensure_clip_bytes(retained_before, semantic_bytes, work)?;
            let semantic_working_bytes = incoming_bytes
                .checked_add(semantic_bytes)
                .ok_or(GeometryFailure::NumericOverflow)?;
            work.ensure_working_bytes(semantic_working_bytes)?;
            work.preflight_fuel(pixels)?;
            work.check_cancellation()?;

            let mut replacement = Vec::new();
            reserve(&mut replacement, current.len(), semantic_bytes)?;
            let replacement_bytes = capacity_bytes::<u64>(replacement.capacity())?;
            let transient_retained = retained_before
                .checked_add(replacement_bytes)
                .ok_or(GeometryFailure::NumericOverflow)?;
            self.operation_peak_retained_bytes =
                self.operation_peak_retained_bytes.max(transient_retained);
            self.peak_retained_bytes = self.peak_retained_bytes.max(transient_retained);
            let actual_working_bytes = incoming_bytes
                .checked_add(replacement_bytes)
                .ok_or(GeometryFailure::NumericOverflow)?;
            work.note_working_bytes(actual_working_bytes)?;
            self.ensure_clip_bytes(retained_before, replacement_bytes, work)?;
            work.ensure_working_bytes(actual_working_bytes)?;
            for (samples_chunk, clip_chunk) in mask
                .samples
                .chunks(MASK_INITIALIZATION_CHUNK_PIXELS)
                .zip(current.chunks(MASK_INITIALIZATION_CHUNK_PIXELS))
            {
                work.charge_fuel(
                    u64::try_from(samples_chunk.len())
                        .map_err(|_| GeometryFailure::NumericOverflow)?,
                )?;
                replacement.extend(
                    samples_chunk
                        .iter()
                        .zip(clip_chunk)
                        .map(|(samples, clip)| *samples & *clip),
                );
            }
            mask.samples = replacement;
        }
        Ok(())
    }

    pub(crate) fn sample_mask(&self, x: u32, y: u32) -> Option<u64> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let index = u64::from(y)
            .checked_mul(u64::from(self.width))?
            .checked_add(u64::from(x))?;
        let index = usize::try_from(index).ok()?;
        Some(
            self.current
                .as_ref()
                .map_or(FULL_SAMPLE_MASK, |mask| mask[index]),
        )
    }

    pub(crate) const fn depth(&self) -> usize {
        self.saved.len()
    }

    pub(crate) fn retained_bytes(&self) -> Result<u64, GeometryFailure> {
        Ok(self.retained_bytes)
    }

    pub(crate) const fn peak_retained_bytes(&self) -> u64 {
        self.peak_retained_bytes
    }

    #[allow(
        dead_code,
        reason = "mounted dispatch aggregates operation-local clip peaks"
    )]
    pub(crate) const fn operation_peak_retained_bytes(&self) -> u64 {
        self.operation_peak_retained_bytes
    }

    fn begin_operation(&mut self) {
        self.operation_peak_retained_bytes = self.retained_bytes;
    }

    fn observe_operation_peak(&mut self, retained_bytes: u64) {
        self.operation_peak_retained_bytes = self.operation_peak_retained_bytes.max(retained_bytes);
        self.peak_retained_bytes = self.peak_retained_bytes.max(retained_bytes);
    }

    fn ensure_clip_bytes(
        &self,
        consumed: u64,
        additional: u64,
        work: &GeometryWork<'_>,
    ) -> Result<(), GeometryFailure> {
        let attempted = consumed
            .checked_add(additional)
            .ok_or(GeometryFailure::NumericOverflow)?;
        if attempted > work.limits().max_clip_bytes {
            return Err(GeometryFailure::Limit {
                kind: GeometryLimitKind::ClipBytes,
                limit: work.limits().max_clip_bytes,
                consumed,
                attempted: additional,
            });
        }
        Ok(())
    }

    #[cfg(test)]
    fn recompute_retained_bytes(&self) -> Result<u64, GeometryFailure> {
        let saved_entries = u64::try_from(self.saved.capacity())
            .map_err(|_| GeometryFailure::NumericOverflow)
            .and_then(|capacity| {
                capacity
                    .checked_mul(
                        u64::try_from(std::mem::size_of::<Option<Vec<u64>>>())
                            .map_err(|_| GeometryFailure::NumericOverflow)?,
                    )
                    .ok_or(GeometryFailure::NumericOverflow)
            })?;
        let mut retained = saved_entries;
        if let Some(current) = &self.current {
            retained = retained
                .checked_add(capacity_bytes::<u64>(current.capacity())?)
                .ok_or(GeometryFailure::NumericOverflow)?;
        }
        for saved in self.saved.iter().flatten() {
            retained = retained
                .checked_add(capacity_bytes::<u64>(saved.capacity())?)
                .ok_or(GeometryFailure::NumericOverflow)?;
        }
        Ok(retained)
    }
}

fn capacity_bytes<T>(capacity: usize) -> Result<u64, GeometryFailure> {
    u64::try_from(capacity)
        .map_err(|_| GeometryFailure::NumericOverflow)?
        .checked_mul(
            u64::try_from(std::mem::size_of::<T>())
                .map_err(|_| GeometryFailure::NumericOverflow)?,
        )
        .ok_or(GeometryFailure::NumericOverflow)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use pdf_rs_scene::{FillRule, PathResource, PathSegment, ScenePoint, SceneScalar};

    use super::{ClipStack, FULL_SAMPLE_MASK, FillEdges, rasterize_fill};
    use crate::reference::geometry::{
        Affine, Fixed, GeometryCancellation, GeometryFailure, GeometryLimitKind, GeometryLimits,
        GeometryWork, flatten_path,
    };

    struct NeverCancel;

    impl GeometryCancellation for NeverCancel {
        fn is_cancelled(&self) -> bool {
            false
        }
    }

    struct Cancellation {
        cancel_at: u64,
        calls: AtomicU64,
    }

    impl Cancellation {
        fn at(call: u64) -> Self {
            Self {
                cancel_at: call,
                calls: AtomicU64::new(0),
            }
        }
    }

    impl GeometryCancellation for Cancellation {
        fn is_cancelled(&self) -> bool {
            self.calls.fetch_add(1, Ordering::SeqCst) + 1 >= self.cancel_at
        }
    }

    fn scalar(value: &str) -> SceneScalar {
        SceneScalar::from_decimal(value).unwrap()
    }

    fn point(x: &str, y: &str) -> ScenePoint {
        ScenePoint::new(scalar(x), scalar(y))
    }

    fn rectangle(left: &str, top: &str, right: &str, bottom: &str) -> Vec<PathSegment> {
        vec![
            PathSegment::MoveTo(point(left, top)),
            PathSegment::LineTo(point(right, top)),
            PathSegment::LineTo(point(right, bottom)),
            PathSegment::LineTo(point(left, bottom)),
            PathSegment::ClosePath,
        ]
    }

    fn edges(
        segments: Vec<PathSegment>,
        work: &mut GeometryWork<'_>,
    ) -> Result<FillEdges, GeometryFailure> {
        let path = PathResource::new(segments).unwrap();
        let flat = flatten_path(
            &path,
            Affine::IDENTITY,
            Affine::IDENTITY,
            Fixed::from_raw(Fixed::ONE.raw() / 256),
            16,
            work,
        )?;
        FillEdges::from_path(&flat, work)
    }

    fn assert_clip_retained_cache(clips: &ClipStack) {
        assert_eq!(
            clips.retained_bytes().unwrap(),
            clips.recompute_retained_bytes().unwrap()
        );
    }

    fn full_mask(width: u32, height: u32, samples: u64) -> super::CoverageMask {
        super::CoverageMask {
            width,
            height,
            samples: vec![samples; usize::try_from(u64::from(width) * u64::from(height)).unwrap()],
        }
    }

    fn clips_with_current(width: u32, height: u32, samples: u64) -> ClipStack {
        let cancellation = NeverCancel;
        let mut work = GeometryWork::new(GeometryLimits::default(), &cancellation).unwrap();
        let mut clips = ClipStack::new(width, height).unwrap();
        clips
            .intersect(full_mask(width, height, samples), &mut work)
            .unwrap();
        clips
    }

    #[test]
    fn eight_by_eight_scalar_coverage_is_exact_on_small_grid() {
        let cancellation = NeverCancel;
        let mut work = GeometryWork::new(GeometryLimits::default(), &cancellation).unwrap();
        let edges = edges(rectangle("0", "0", "1", "1"), &mut work).unwrap();
        let mask = rasterize_fill(&edges, FillRule::Nonzero, 2, 2, &mut work).unwrap();
        assert_eq!(mask.coverage(0, 0), Some(64));
        assert_eq!(mask.sample_mask(0, 0), Some(FULL_SAMPLE_MASK));
        assert_eq!(mask.coverage(1, 0), Some(0));
        assert_eq!(mask.coverage(0, 1), Some(0));
        assert_eq!(mask.coverage(1, 1), Some(0));
        assert_eq!(work.samples(), 64);
    }

    #[test]
    fn analytic_half_pixel_triangle_has_twenty_eight_owned_samples() {
        let cancellation = NeverCancel;
        let mut work = GeometryWork::new(GeometryLimits::default(), &cancellation).unwrap();
        let triangle = vec![
            PathSegment::MoveTo(point("0", "0")),
            PathSegment::LineTo(point("1", "0")),
            PathSegment::LineTo(point("0", "1")),
            PathSegment::ClosePath,
        ];
        let edges = edges(triangle, &mut work).unwrap();
        let mask = rasterize_fill(&edges, FillRule::Nonzero, 1, 1, &mut work).unwrap();
        assert_eq!(mask.coverage(0, 0), Some(28));
    }

    #[test]
    fn separated_lone_moves_do_not_expand_fill_bounds_or_consume_samples() {
        let cancellation = NeverCancel;
        let mut work = GeometryWork::new(
            GeometryLimits {
                max_samples: 1,
                ..GeometryLimits::default()
            },
            &cancellation,
        )
        .unwrap();
        let fill_edges = edges(
            vec![
                PathSegment::MoveTo(point("-1000000", "-1000000")),
                PathSegment::MoveTo(point("1000000", "1000000")),
            ],
            &mut work,
        )
        .unwrap();
        assert!(fill_edges.edges.is_empty());

        let mask = rasterize_fill(&fill_edges, FillRule::Nonzero, 32, 32, &mut work).unwrap();
        assert_eq!(work.samples(), 0);
        assert!(mask.samples().iter().all(|samples| *samples == 0));
    }

    #[test]
    fn nonzero_and_evenodd_diverge_for_same_orientation_nested_subpaths() {
        let mut segments = rectangle("0", "0", "3", "3");
        segments.extend(rectangle("1", "1", "2", "2"));
        let cancellation = NeverCancel;
        let mut nonzero_work = GeometryWork::new(GeometryLimits::default(), &cancellation).unwrap();
        let nonzero_edges = edges(segments.clone(), &mut nonzero_work).unwrap();
        let nonzero =
            rasterize_fill(&nonzero_edges, FillRule::Nonzero, 3, 3, &mut nonzero_work).unwrap();
        let mut evenodd_work = GeometryWork::new(GeometryLimits::default(), &cancellation).unwrap();
        let evenodd_edges = edges(segments, &mut evenodd_work).unwrap();
        let evenodd =
            rasterize_fill(&evenodd_edges, FillRule::EvenOdd, 3, 3, &mut evenodd_work).unwrap();
        assert_eq!(nonzero.coverage(1, 1), Some(64));
        assert_eq!(evenodd.coverage(1, 1), Some(0));
        assert_eq!(nonzero.coverage(0, 0), Some(64));
        assert_eq!(evenodd.coverage(0, 0), Some(64));
    }

    #[test]
    fn reversing_a_simple_subpath_preserves_fill_coverage() {
        let clockwise = rectangle("0", "0", "2", "2");
        let counterclockwise = vec![
            PathSegment::MoveTo(point("0", "0")),
            PathSegment::LineTo(point("0", "2")),
            PathSegment::LineTo(point("2", "2")),
            PathSegment::LineTo(point("2", "0")),
            PathSegment::ClosePath,
        ];
        let cancellation = NeverCancel;
        for rule in [FillRule::Nonzero, FillRule::EvenOdd] {
            let mut clockwise_work =
                GeometryWork::new(GeometryLimits::default(), &cancellation).unwrap();
            let clockwise_edges = edges(clockwise.clone(), &mut clockwise_work).unwrap();
            let clockwise_mask =
                rasterize_fill(&clockwise_edges, rule, 2, 2, &mut clockwise_work).unwrap();
            let mut counterclockwise_work =
                GeometryWork::new(GeometryLimits::default(), &cancellation).unwrap();
            let counterclockwise_edges =
                edges(counterclockwise.clone(), &mut counterclockwise_work).unwrap();
            let counterclockwise_mask = rasterize_fill(
                &counterclockwise_edges,
                rule,
                2,
                2,
                &mut counterclockwise_work,
            )
            .unwrap();
            assert_eq!(clockwise_mask, counterclockwise_mask);
        }
    }

    #[test]
    fn half_open_vertex_rule_does_not_double_count_shared_extrema() {
        let cancellation = NeverCancel;
        let mut work = GeometryWork::new(GeometryLimits::default(), &cancellation).unwrap();
        let diamond = vec![
            PathSegment::MoveTo(point("1", "0")),
            PathSegment::LineTo(point("2", "1")),
            PathSegment::LineTo(point("1", "2")),
            PathSegment::LineTo(point("0", "1")),
            PathSegment::ClosePath,
        ];
        let edges = edges(diamond, &mut work).unwrap();
        assert!(
            !edges
                .contains(
                    crate::reference::geometry::FixedPoint::new(
                        Fixed::from_i64(2).unwrap(),
                        Fixed::from_i64(1).unwrap()
                    ),
                    FillRule::EvenOdd,
                    &mut work,
                )
                .unwrap()
        );
        assert!(
            edges
                .contains(
                    crate::reference::geometry::FixedPoint::new(
                        Fixed::from_i64(1).unwrap(),
                        Fixed::from_i64(1).unwrap()
                    ),
                    FillRule::EvenOdd,
                    &mut work,
                )
                .unwrap()
        );
    }

    #[test]
    fn clip_intersection_save_and_restore_preserve_exact_sample_masks() {
        let cancellation = NeverCancel;
        let mut work = GeometryWork::new(GeometryLimits::default(), &cancellation).unwrap();
        let left_edges = edges(rectangle("0", "0", "1", "2"), &mut work).unwrap();
        let left = rasterize_fill(&left_edges, FillRule::Nonzero, 2, 2, &mut work).unwrap();
        let top_edges = edges(rectangle("0", "0", "2", "1"), &mut work).unwrap();
        let top = rasterize_fill(&top_edges, FillRule::Nonzero, 2, 2, &mut work).unwrap();
        let mut clips = ClipStack::new(2, 2).unwrap();
        clips.intersect(left, &mut work).unwrap();
        assert_eq!(clips.sample_mask(0, 0), Some(FULL_SAMPLE_MASK));
        assert_eq!(clips.sample_mask(1, 0), Some(0));
        clips.save(&mut work).unwrap();
        clips.intersect(top, &mut work).unwrap();
        assert_eq!(clips.sample_mask(0, 0), Some(FULL_SAMPLE_MASK));
        assert_eq!(clips.sample_mask(0, 1), Some(0));
        let retained_while_saved = clips.retained_bytes().unwrap();
        clips.restore(&mut work).unwrap();
        assert_eq!(clips.sample_mask(0, 1), Some(FULL_SAMPLE_MASK));
        assert_eq!(clips.depth(), 0);
        assert!(clips.retained_bytes().unwrap() < retained_while_saved);
        assert!(clips.retained_bytes().unwrap() >= left_mask_bytes(2, 2));
        assert!(clips.peak_retained_bytes() >= retained_while_saved);
    }

    #[test]
    fn deep_clip_stack_growth_is_charged_once_and_retained_queries_are_constant_work() {
        let cancellation = NeverCancel;
        let depth = 64_u64;
        let mut work = GeometryWork::new(
            GeometryLimits {
                max_clip_depth: u32::try_from(depth).unwrap(),
                ..GeometryLimits::default()
            },
            &cancellation,
        )
        .unwrap();
        let mut clips = ClipStack::new(1, 1).unwrap();
        for _ in 0..depth {
            clips.save(&mut work).unwrap();
            assert_clip_retained_cache(&clips);
        }
        assert!(work.fuel() >= depth);
        assert!(work.fuel() <= depth * 2);

        let query_fuel = work.fuel();
        for _ in 0..1024 {
            assert_clip_retained_cache(&clips);
        }
        assert_eq!(work.fuel(), query_fuel);

        let retained_before_failure = clips.retained_bytes().unwrap();
        assert!(matches!(
            clips.save(&mut work),
            Err(GeometryFailure::Limit {
                kind: GeometryLimitKind::ClipDepth,
                limit: 64,
                consumed: 64,
                attempted: 1
            })
        ));
        assert_eq!(clips.retained_bytes().unwrap(), retained_before_failure);
        assert_clip_retained_cache(&clips);

        for _ in 0..depth {
            clips.restore(&mut work).unwrap();
            assert_clip_retained_cache(&clips);
        }
        assert_eq!(clips.depth(), 0);
    }

    #[test]
    fn allocator_overcapacity_does_not_change_clip_fuel_schedule() {
        fn run(
            extra_capacity: usize,
            max_fuel: u64,
        ) -> (Result<(), GeometryFailure>, usize, u64, u64, u64) {
            let cancellation = NeverCancel;
            let mut work = GeometryWork::new(
                GeometryLimits {
                    max_clip_depth: 300,
                    max_fuel,
                    ..GeometryLimits::default()
                },
                &cancellation,
            )
            .unwrap();
            let mut clips = ClipStack::new(1, 1).unwrap();
            let mut result = Ok(());
            for _ in 0..257 {
                result = clips.save_with_outer_reserve(
                    &mut work,
                    |replacement, target_capacity, target_bytes| {
                        replacement
                            .try_reserve_exact(target_capacity.checked_add(extra_capacity).unwrap())
                            .map_err(|_| GeometryFailure::Allocation {
                                attempted_bytes: target_bytes,
                            })
                    },
                );
                if result.is_err() {
                    break;
                }
            }
            (
                result,
                clips.depth(),
                work.fuel(),
                work.cancellation_checks(),
                clips.retained_bytes().unwrap(),
            )
        }

        let exact = run(0, GeometryLimits::default().max_fuel);
        let overcapacity = run(2, GeometryLimits::default().max_fuel);
        assert_eq!(exact.0, Ok(()));
        assert_eq!(exact.0, overcapacity.0);
        assert_eq!(exact.1, overcapacity.1);
        assert_eq!(exact.2, overcapacity.2);
        assert_eq!(exact.3, overcapacity.3);

        let one_less_fuel = exact.2.checked_sub(1).unwrap();
        let exact_limited = run(0, one_less_fuel);
        let overcapacity_limited = run(2, one_less_fuel);
        assert_eq!(exact_limited.0, overcapacity_limited.0);
        assert_eq!(exact_limited.1, overcapacity_limited.1);
        assert_eq!(exact_limited.2, overcapacity_limited.2);
        assert_eq!(exact_limited.3, overcapacity_limited.3);
        assert!(matches!(
            exact_limited.0,
            Err(GeometryFailure::Limit {
                kind: GeometryLimitKind::Fuel,
                ..
            })
        ));
    }

    #[test]
    fn clip_actual_overcapacity_postflight_records_failure_peaks_transactionally() {
        let cancellation = NeverCancel;

        let target_outer_capacity = crate::reference::geometry::logical_vector_capacity(1).unwrap();
        let semantic_outer_bytes =
            super::capacity_bytes::<Option<Vec<u64>>>(target_outer_capacity).unwrap();
        let mut save_work = GeometryWork::new(
            GeometryLimits {
                max_clip_bytes: semantic_outer_bytes,
                ..GeometryLimits::default()
            },
            &cancellation,
        )
        .unwrap();
        let mut save_clips = ClipStack::new(1, 1).unwrap();
        let mut actual_outer_bytes = 0;
        let save_failure = save_clips
            .save_with_outer_reserve(
                &mut save_work,
                |replacement, target_capacity, target_bytes| {
                    replacement
                        .try_reserve_exact(target_capacity.checked_add(1).unwrap())
                        .map_err(|_| GeometryFailure::Allocation {
                            attempted_bytes: target_bytes,
                        })?;
                    actual_outer_bytes =
                        super::capacity_bytes::<Option<Vec<u64>>>(replacement.capacity())?;
                    Ok(())
                },
            )
            .unwrap_err();
        assert!(actual_outer_bytes > semantic_outer_bytes);
        assert!(matches!(
            save_failure,
            GeometryFailure::Limit {
                kind: GeometryLimitKind::ClipBytes,
                limit,
                consumed: 0,
                attempted,
            } if limit == semantic_outer_bytes && attempted == actual_outer_bytes
        ));
        assert_eq!(save_clips.depth(), 0);
        assert_eq!(save_clips.retained_bytes().unwrap(), 0);
        assert_eq!(
            save_clips.operation_peak_retained_bytes(),
            actual_outer_bytes
        );
        assert_eq!(save_clips.peak_retained_bytes(), actual_outer_bytes);
        assert_eq!(save_work.peak_working_bytes(), actual_outer_bytes);

        let mut intersect_clips = clips_with_current(1, 1, FULL_SAMPLE_MASK);
        let intersect_retained = intersect_clips.retained_bytes().unwrap();
        let incoming = full_mask(1, 1, 0);
        let incoming_bytes = incoming.retained_bytes().unwrap();
        let semantic_replacement_bytes = u64::try_from(std::mem::size_of::<u64>()).unwrap();
        let intersect_limit = intersect_retained
            .checked_add(semantic_replacement_bytes)
            .unwrap();
        let mut intersect_work = GeometryWork::new(
            GeometryLimits {
                max_clip_bytes: intersect_limit,
                ..GeometryLimits::default()
            },
            &cancellation,
        )
        .unwrap();
        let mut actual_intersection_bytes = 0;
        let intersect_failure = intersect_clips
            .intersect_with_reserve(
                incoming,
                &mut intersect_work,
                |replacement, target_capacity, target_bytes| {
                    replacement
                        .try_reserve_exact(target_capacity.checked_add(1).unwrap())
                        .map_err(|_| GeometryFailure::Allocation {
                            attempted_bytes: target_bytes,
                        })?;
                    actual_intersection_bytes =
                        super::capacity_bytes::<u64>(replacement.capacity())?;
                    Ok(())
                },
            )
            .unwrap_err();
        assert!(actual_intersection_bytes > semantic_replacement_bytes);
        assert!(matches!(
            intersect_failure,
            GeometryFailure::Limit {
                kind: GeometryLimitKind::ClipBytes,
                limit,
                consumed,
                attempted,
            } if limit == intersect_limit
                && consumed == intersect_retained
                && attempted == actual_intersection_bytes
        ));
        let intersect_peak = intersect_retained
            .checked_add(actual_intersection_bytes)
            .unwrap();
        assert_eq!(
            intersect_clips.retained_bytes().unwrap(),
            intersect_retained
        );
        assert_eq!(intersect_clips.sample_mask(0, 0), Some(FULL_SAMPLE_MASK));
        assert_eq!(
            intersect_clips.operation_peak_retained_bytes(),
            intersect_peak
        );
        assert_eq!(intersect_clips.peak_retained_bytes(), intersect_peak);
        assert_eq!(
            intersect_work.peak_working_bytes(),
            actual_intersection_bytes
                .checked_add(incoming_bytes)
                .unwrap()
        );
        assert_eq!(
            intersect_retained
                .checked_add(intersect_work.peak_working_bytes())
                .unwrap(),
            intersect_peak.checked_add(incoming_bytes).unwrap()
        );

        let mut apply_clips = clips_with_current(1, 1, 0);
        let apply_retained = apply_clips.retained_bytes().unwrap();
        let apply_limit = apply_retained
            .checked_add(semantic_replacement_bytes)
            .unwrap();
        let mut apply_work = GeometryWork::new(
            GeometryLimits {
                max_clip_bytes: apply_limit,
                ..GeometryLimits::default()
            },
            &cancellation,
        )
        .unwrap();
        let mut target = full_mask(1, 1, FULL_SAMPLE_MASK);
        let target_bytes = target.retained_bytes().unwrap();
        let target_before = target.clone();
        let mut actual_apply_bytes = 0;
        let apply_failure = apply_clips
            .apply_with_reserve(
                &mut target,
                &mut apply_work,
                |replacement, target_capacity, target_bytes| {
                    replacement
                        .try_reserve_exact(target_capacity.checked_add(1).unwrap())
                        .map_err(|_| GeometryFailure::Allocation {
                            attempted_bytes: target_bytes,
                        })?;
                    actual_apply_bytes = super::capacity_bytes::<u64>(replacement.capacity())?;
                    Ok(())
                },
            )
            .unwrap_err();
        assert!(actual_apply_bytes > semantic_replacement_bytes);
        assert!(matches!(
            apply_failure,
            GeometryFailure::Limit {
                kind: GeometryLimitKind::ClipBytes,
                limit,
                consumed,
                attempted,
            } if limit == apply_limit
                && consumed == apply_retained
                && attempted == actual_apply_bytes
        ));
        let apply_peak = apply_retained.checked_add(actual_apply_bytes).unwrap();
        assert_eq!(target, target_before);
        assert_eq!(apply_clips.retained_bytes().unwrap(), apply_retained);
        assert_eq!(apply_clips.operation_peak_retained_bytes(), apply_peak);
        assert_eq!(apply_clips.peak_retained_bytes(), apply_peak);
        assert_eq!(
            apply_work.peak_working_bytes(),
            target_bytes.checked_add(actual_apply_bytes).unwrap()
        );
        assert_eq!(
            apply_retained
                .checked_add(apply_work.peak_working_bytes())
                .unwrap(),
            apply_peak.checked_add(target_bytes).unwrap()
        );
    }

    #[test]
    fn clip_apply_semantic_working_one_less_rejects_before_allocation_transactionally() {
        let cancellation = NeverCancel;
        let mut clips = clips_with_current(1, 1, 0);
        let retained_before = clips.retained_bytes().unwrap();
        let peak_before = clips.peak_retained_bytes();
        let mut target = full_mask(1, 1, FULL_SAMPLE_MASK);
        let target_before = target.clone();
        let target_bytes = target.retained_bytes().unwrap();
        let semantic_replacement_bytes = u64::try_from(std::mem::size_of::<u64>()).unwrap();
        let semantic_working_bytes = target_bytes
            .checked_add(semantic_replacement_bytes)
            .unwrap();
        let mut work = GeometryWork::new(
            GeometryLimits {
                max_working_bytes: semantic_working_bytes - 1,
                ..GeometryLimits::default()
            },
            &cancellation,
        )
        .unwrap();

        assert_eq!(
            clips.apply(&mut target, &mut work),
            Err(GeometryFailure::Limit {
                kind: GeometryLimitKind::WorkingBytes,
                limit: semantic_working_bytes - 1,
                consumed: 0,
                attempted: semantic_working_bytes,
            })
        );
        assert_eq!(target, target_before);
        assert_eq!(clips.retained_bytes().unwrap(), retained_before);
        assert_eq!(clips.operation_peak_retained_bytes(), retained_before);
        assert_eq!(clips.peak_retained_bytes(), peak_before);
        assert_eq!(work.peak_working_bytes(), 0);
    }

    #[test]
    fn clip_apply_actual_overcapacity_working_postflight_records_peak_transactionally() {
        let cancellation = NeverCancel;
        let mut clips = clips_with_current(1, 1, 0);
        let retained_before = clips.retained_bytes().unwrap();
        let mut target = full_mask(1, 1, FULL_SAMPLE_MASK);
        let target_bytes = target.retained_bytes().unwrap();
        let target_before = target.clone();
        let semantic_replacement_bytes = u64::try_from(std::mem::size_of::<u64>()).unwrap();
        let semantic_working_bytes = target_bytes
            .checked_add(semantic_replacement_bytes)
            .unwrap();
        let mut work = GeometryWork::new(
            GeometryLimits {
                max_working_bytes: semantic_working_bytes,
                ..GeometryLimits::default()
            },
            &cancellation,
        )
        .unwrap();
        let mut actual_replacement_bytes = 0;

        let failure = clips
            .apply_with_reserve(
                &mut target,
                &mut work,
                |replacement, target_capacity, target_bytes| {
                    replacement
                        .try_reserve_exact(target_capacity.checked_add(1).unwrap())
                        .map_err(|_| GeometryFailure::Allocation {
                            attempted_bytes: target_bytes,
                        })?;
                    actual_replacement_bytes =
                        super::capacity_bytes::<u64>(replacement.capacity())?;
                    Ok(())
                },
            )
            .unwrap_err();
        let actual_working_bytes = target_bytes.checked_add(actual_replacement_bytes).unwrap();
        assert!(actual_replacement_bytes > semantic_replacement_bytes);
        assert_eq!(
            failure,
            GeometryFailure::Limit {
                kind: GeometryLimitKind::WorkingBytes,
                limit: semantic_working_bytes,
                consumed: 0,
                attempted: actual_working_bytes,
            }
        );
        assert_eq!(target, target_before);
        assert_eq!(clips.retained_bytes().unwrap(), retained_before);
        assert_eq!(
            clips.operation_peak_retained_bytes(),
            retained_before
                .checked_add(actual_replacement_bytes)
                .unwrap()
        );
        assert_eq!(
            clips.peak_retained_bytes(),
            clips.operation_peak_retained_bytes()
        );
        assert_eq!(work.peak_working_bytes(), actual_working_bytes);
    }

    #[test]
    fn clip_cancellation_after_temporary_allocation_records_failure_peaks_transactionally() {
        let mut save_clips = clips_with_current(16, 16, FULL_SAMPLE_MASK);
        let save_retained = save_clips.retained_bytes().unwrap();
        let cancellation = Cancellation::at(3);
        let mut save_work = GeometryWork::new(GeometryLimits::default(), &cancellation).unwrap();
        assert_eq!(
            save_clips.save(&mut save_work),
            Err(GeometryFailure::Cancelled)
        );
        let save_peak = save_clips.operation_peak_retained_bytes();
        assert!(save_peak > save_retained);
        assert_eq!(save_clips.retained_bytes().unwrap(), save_retained);
        assert_eq!(save_clips.depth(), 0);
        assert_eq!(save_clips.peak_retained_bytes(), save_peak);
        assert_eq!(
            save_retained
                .checked_add(save_work.peak_working_bytes())
                .unwrap(),
            save_peak
        );

        let mut intersect_clips = clips_with_current(16, 16, FULL_SAMPLE_MASK);
        let intersect_retained = intersect_clips.retained_bytes().unwrap();
        let incoming = full_mask(16, 16, 0);
        let incoming_bytes = incoming.retained_bytes().unwrap();
        let cancellation = Cancellation::at(3);
        let mut intersect_work =
            GeometryWork::new(GeometryLimits::default(), &cancellation).unwrap();
        assert_eq!(
            intersect_clips.intersect(incoming, &mut intersect_work),
            Err(GeometryFailure::Cancelled)
        );
        let intersect_peak = intersect_clips.operation_peak_retained_bytes();
        assert!(intersect_peak > intersect_retained);
        assert_eq!(
            intersect_clips.retained_bytes().unwrap(),
            intersect_retained
        );
        assert_eq!(intersect_clips.sample_mask(0, 0), Some(FULL_SAMPLE_MASK));
        assert_eq!(intersect_clips.peak_retained_bytes(), intersect_peak);
        assert_eq!(
            intersect_work.peak_working_bytes(),
            intersect_peak
                .checked_sub(intersect_retained)
                .and_then(|replacement| replacement.checked_add(incoming_bytes))
                .unwrap()
        );

        let mut apply_clips = clips_with_current(16, 16, 0);
        let apply_retained = apply_clips.retained_bytes().unwrap();
        let cancellation = Cancellation::at(3);
        let mut apply_work = GeometryWork::new(GeometryLimits::default(), &cancellation).unwrap();
        let mut target = full_mask(16, 16, FULL_SAMPLE_MASK);
        let target_bytes = target.retained_bytes().unwrap();
        let target_before = target.clone();
        assert_eq!(
            apply_clips.apply(&mut target, &mut apply_work),
            Err(GeometryFailure::Cancelled)
        );
        let apply_peak = apply_clips.operation_peak_retained_bytes();
        assert!(apply_peak > apply_retained);
        assert_eq!(target, target_before);
        assert_eq!(apply_clips.retained_bytes().unwrap(), apply_retained);
        assert_eq!(apply_clips.peak_retained_bytes(), apply_peak);
        assert_eq!(
            apply_retained
                .checked_add(apply_work.peak_working_bytes())
                .unwrap(),
            apply_peak.checked_add(target_bytes).unwrap()
        );
    }

    #[test]
    fn clip_retained_cache_stays_exact_across_replacement_restore_and_cancellation() {
        let never = NeverCancel;
        let mut work = GeometryWork::new(GeometryLimits::default(), &never).unwrap();
        let mut clips = ClipStack::new(2, 2).unwrap();
        clips
            .intersect(
                super::CoverageMask {
                    width: 2,
                    height: 2,
                    samples: vec![FULL_SAMPLE_MASK; 4],
                },
                &mut work,
            )
            .unwrap();
        assert_clip_retained_cache(&clips);
        clips.save(&mut work).unwrap();
        assert_clip_retained_cache(&clips);
        clips
            .intersect(
                super::CoverageMask {
                    width: 2,
                    height: 2,
                    samples: vec![0; 4],
                },
                &mut work,
            )
            .unwrap();
        assert_clip_retained_cache(&clips);

        let retained_before_cancellation = clips.retained_bytes().unwrap();
        let depth_before_cancellation = clips.depth();
        let sample_before_cancellation = clips.sample_mask(0, 0);
        let cancellation = Cancellation::at(2);
        let mut cancelled_work =
            GeometryWork::new(GeometryLimits::default(), &cancellation).unwrap();
        assert_eq!(
            clips.save(&mut cancelled_work),
            Err(GeometryFailure::Cancelled)
        );
        assert_eq!(
            clips.retained_bytes().unwrap(),
            retained_before_cancellation
        );
        assert_eq!(clips.depth(), depth_before_cancellation);
        assert_eq!(clips.sample_mask(0, 0), sample_before_cancellation);
        assert_clip_retained_cache(&clips);

        clips.restore(&mut work).unwrap();
        assert_clip_retained_cache(&clips);
        assert_eq!(clips.sample_mask(0, 0), Some(FULL_SAMPLE_MASK));
    }

    #[test]
    fn one_less_edge_sample_depth_and_clip_byte_budgets_fail_closed() {
        let cancellation = NeverCancel;
        let mut edge_work = GeometryWork::new(
            GeometryLimits {
                max_edges: 3,
                ..GeometryLimits::default()
            },
            &cancellation,
        )
        .unwrap();
        assert!(matches!(
            edges(rectangle("0", "0", "1", "1"), &mut edge_work),
            Err(GeometryFailure::Limit {
                kind: GeometryLimitKind::Edges,
                limit: 3,
                consumed: 3,
                attempted: 1
            })
        ));

        let mut sample_work = GeometryWork::new(
            GeometryLimits {
                max_samples: 63,
                ..GeometryLimits::default()
            },
            &cancellation,
        )
        .unwrap();
        let fill_edges = edges(rectangle("0", "0", "1", "1"), &mut sample_work).unwrap();
        assert!(matches!(
            rasterize_fill(&fill_edges, FillRule::Nonzero, 1, 1, &mut sample_work),
            Err(GeometryFailure::Limit {
                kind: GeometryLimitKind::Samples,
                limit: 63,
                consumed: 0,
                attempted: 64
            })
        ));

        let mut depth_work = GeometryWork::new(
            GeometryLimits {
                max_clip_depth: 1,
                ..GeometryLimits::default()
            },
            &cancellation,
        )
        .unwrap();
        let mut clips = ClipStack::new(1, 1).unwrap();
        clips.save(&mut depth_work).unwrap();
        assert!(matches!(
            clips.save(&mut depth_work),
            Err(GeometryFailure::Limit {
                kind: GeometryLimitKind::ClipDepth,
                limit: 1,
                consumed: 1,
                attempted: 1
            })
        ));

        let mut bytes_work = GeometryWork::new(
            GeometryLimits {
                max_clip_bytes: 7,
                ..GeometryLimits::default()
            },
            &cancellation,
        )
        .unwrap();
        let mut clips = ClipStack::new(1, 1).unwrap();
        let full = super::CoverageMask {
            width: 1,
            height: 1,
            samples: vec![FULL_SAMPLE_MASK],
        };
        assert!(matches!(
            clips.intersect(full, &mut bytes_work),
            Err(GeometryFailure::Limit {
                kind: GeometryLimitKind::ClipBytes,
                limit: 7,
                consumed: 0,
                attempted: 8
            })
        ));
    }

    #[test]
    fn coverage_and_comparison_fuel_are_admitted_before_mask_allocation() {
        let cancellation = NeverCancel;
        let mut build_work = GeometryWork::new(GeometryLimits::default(), &cancellation).unwrap();
        let fill_edges = edges(rectangle("0", "0", "1", "1"), &mut build_work).unwrap();

        let mut coverage_work = GeometryWork::new(
            GeometryLimits {
                max_coverage_bytes: 7,
                ..GeometryLimits::default()
            },
            &cancellation,
        )
        .unwrap();
        assert!(matches!(
            rasterize_fill(&fill_edges, FillRule::Nonzero, 1, 1, &mut coverage_work),
            Err(GeometryFailure::Limit {
                kind: GeometryLimitKind::CoverageBytes,
                limit: 7,
                consumed: 0,
                attempted: 8
            })
        ));
        assert_eq!(coverage_work.samples(), 0);

        let total_fuel = 64_u64
            .checked_mul(2)
            .and_then(|value| value.checked_add(64))
            .and_then(|value| value.checked_add(1))
            .unwrap();
        let mut fuel_work = GeometryWork::new(
            GeometryLimits {
                max_fuel: total_fuel - 1,
                ..GeometryLimits::default()
            },
            &cancellation,
        )
        .unwrap();
        assert!(matches!(
            rasterize_fill(&fill_edges, FillRule::Nonzero, 1, 1, &mut fuel_work),
            Err(GeometryFailure::Limit {
                kind: GeometryLimitKind::Fuel,
                limit,
                consumed: 0,
                attempted
            }) if limit == total_fuel - 1 && attempted == total_fuel
        ));
        assert_eq!(fuel_work.samples(), 0);
    }

    #[test]
    fn full_mask_initialization_is_preflighted_and_cancellable_in_fixed_chunks() {
        let never = NeverCancel;
        let mut limited_work = GeometryWork::new(
            GeometryLimits {
                max_fuel: 255,
                ..GeometryLimits::default()
            },
            &never,
        )
        .unwrap();
        assert!(matches!(
            super::CoverageMask::empty(16, 16, &mut limited_work),
            Err(GeometryFailure::Limit {
                kind: GeometryLimitKind::Fuel,
                limit: 255,
                consumed: 0,
                attempted: 256
            })
        ));
        assert_eq!(limited_work.fuel(), 0);

        let cancellation = Cancellation::at(3);
        let mut cancelled_work =
            GeometryWork::new(GeometryLimits::default(), &cancellation).unwrap();
        assert_eq!(
            super::CoverageMask::empty(16, 16, &mut cancelled_work),
            Err(GeometryFailure::Cancelled)
        );
        assert_eq!(cancelled_work.fuel(), 256);
    }

    #[test]
    fn clip_save_uses_capacity_bytes_and_fails_transactionally_one_byte_short() {
        let cancellation = NeverCancel;
        let mask = super::CoverageMask {
            width: 2,
            height: 2,
            samples: vec![FULL_SAMPLE_MASK; 4],
        };

        let mut measuring_work =
            GeometryWork::new(GeometryLimits::default(), &cancellation).unwrap();
        let mut measuring = ClipStack::new(2, 2).unwrap();
        measuring
            .intersect(mask.clone(), &mut measuring_work)
            .unwrap();
        measuring.save(&mut measuring_work).unwrap();
        let required = measuring.peak_retained_bytes();
        assert!(required > mask.retained_bytes().unwrap());

        let mut limited_work = GeometryWork::new(
            GeometryLimits {
                max_clip_bytes: required - 1,
                ..GeometryLimits::default()
            },
            &cancellation,
        )
        .unwrap();
        let mut limited = ClipStack::new(2, 2).unwrap();
        limited.intersect(mask, &mut limited_work).unwrap();
        let before = limited.retained_bytes().unwrap();
        assert!(matches!(
            limited.save(&mut limited_work),
            Err(GeometryFailure::Limit {
                kind: GeometryLimitKind::ClipBytes,
                ..
            })
        ));
        assert_eq!(limited.depth(), 0);
        assert_eq!(limited.retained_bytes().unwrap(), before);
    }

    #[test]
    fn clip_apply_cancellation_happens_before_mask_mutation() {
        let never = NeverCancel;
        let mut setup_work = GeometryWork::new(GeometryLimits::default(), &never).unwrap();
        let mut clips = ClipStack::new(16, 16).unwrap();
        clips
            .intersect(
                super::CoverageMask {
                    width: 16,
                    height: 16,
                    samples: vec![0; 256],
                },
                &mut setup_work,
            )
            .unwrap();

        let cancellation = Cancellation::at(3);
        let mut apply_work = GeometryWork::new(GeometryLimits::default(), &cancellation).unwrap();
        let mut target = super::CoverageMask {
            width: 16,
            height: 16,
            samples: vec![FULL_SAMPLE_MASK; 256],
        };
        let before = target.clone();
        assert_eq!(
            clips.apply(&mut target, &mut apply_work),
            Err(GeometryFailure::Cancelled)
        );
        assert_eq!(target, before);
    }

    #[test]
    fn clip_intersection_cancellation_preserves_the_previous_mask() {
        let never = NeverCancel;
        let mut setup_work = GeometryWork::new(GeometryLimits::default(), &never).unwrap();
        let mut clips = ClipStack::new(16, 16).unwrap();
        clips
            .intersect(
                super::CoverageMask {
                    width: 16,
                    height: 16,
                    samples: vec![FULL_SAMPLE_MASK; 256],
                },
                &mut setup_work,
            )
            .unwrap();

        let cancellation = Cancellation::at(3);
        let mut intersect_work =
            GeometryWork::new(GeometryLimits::default(), &cancellation).unwrap();
        assert_eq!(
            clips.intersect(
                super::CoverageMask {
                    width: 16,
                    height: 16,
                    samples: vec![0; 256],
                },
                &mut intersect_work,
            ),
            Err(GeometryFailure::Cancelled)
        );
        assert_eq!(clips.sample_mask(0, 0), Some(FULL_SAMPLE_MASK));
        assert_eq!(clips.sample_mask(15, 15), Some(FULL_SAMPLE_MASK));
    }

    fn left_mask_bytes(width: u32, height: u32) -> u64 {
        u64::from(width) * u64::from(height) * 8
    }
}
