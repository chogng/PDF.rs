use pdf_rs_scene::{
    Matrix, PageGeometry, PageRotation, PathResource, PathSegment, ScenePoint, SceneScalar,
};

const FIXED_FRACTION_BITS: u32 = 32;
const FIXED_ONE_I128: i128 = 1_i128 << FIXED_FRACTION_BITS;
const SCENE_SCALE_I128: i128 = 1_000_000_000;
const CANCELLATION_FUEL_INTERVAL: u64 = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GeometryLimitKind {
    CurveRecursion,
    Segments,
    Edges,
    Samples,
    CoverageBytes,
    DashChunks,
    StrokeRuns,
    StrokePrimitives,
    GeometryBytes,
    ClipDepth,
    ClipBytes,
    Fuel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GeometryFailure {
    NumericOverflow,
    InvalidGeometry,
    Cancelled,
    Allocation {
        attempted_bytes: u64,
    },
    Limit {
        kind: GeometryLimitKind,
        limit: u64,
        consumed: u64,
        attempted: u64,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct GeometryLimits {
    pub(crate) max_segments: u64,
    pub(crate) max_edges: u64,
    pub(crate) max_samples: u64,
    pub(crate) max_coverage_bytes: u64,
    pub(crate) max_dash_chunks: u64,
    pub(crate) max_stroke_runs: u64,
    pub(crate) max_stroke_primitives: u64,
    pub(crate) max_geometry_bytes: u64,
    pub(crate) max_clip_depth: u32,
    pub(crate) max_clip_bytes: u64,
    pub(crate) max_fuel: u64,
}

impl Default for GeometryLimits {
    fn default() -> Self {
        Self {
            max_segments: 4_000_000,
            max_edges: 4_000_000,
            max_samples: 1_000_000_000,
            max_coverage_bytes: 256 * 1024 * 1024,
            max_dash_chunks: 4_000_000,
            max_stroke_runs: 1_000_000,
            max_stroke_primitives: 8_000_000,
            max_geometry_bytes: 256 * 1024 * 1024,
            max_clip_depth: 256,
            max_clip_bytes: 256 * 1024 * 1024,
            max_fuel: 1_000_000_000,
        }
    }
}

pub(crate) trait GeometryCancellation {
    fn is_cancelled(&self) -> bool;
}

pub(crate) struct GeometryWork<'a> {
    limits: GeometryLimits,
    cancellation: &'a dyn GeometryCancellation,
    segments: u64,
    edges: u64,
    samples: u64,
    dash_chunks: u64,
    stroke_runs: u64,
    stroke_primitives: u64,
    geometry_bytes: u64,
    peak_geometry_bytes: u64,
    fuel: u64,
    cancellation_checks: u64,
    fuel_since_cancellation: u64,
}

impl<'a> GeometryWork<'a> {
    pub(crate) fn new(
        limits: GeometryLimits,
        cancellation: &'a dyn GeometryCancellation,
    ) -> Result<Self, GeometryFailure> {
        if limits.max_segments == 0
            || limits.max_edges == 0
            || limits.max_samples == 0
            || limits.max_coverage_bytes == 0
            || limits.max_dash_chunks == 0
            || limits.max_stroke_runs == 0
            || limits.max_stroke_primitives == 0
            || limits.max_geometry_bytes == 0
            || limits.max_clip_depth == 0
            || limits.max_clip_bytes == 0
            || limits.max_fuel == 0
        {
            return Err(GeometryFailure::InvalidGeometry);
        }
        let mut work = Self {
            limits,
            cancellation,
            segments: 0,
            edges: 0,
            samples: 0,
            dash_chunks: 0,
            stroke_runs: 0,
            stroke_primitives: 0,
            geometry_bytes: 0,
            peak_geometry_bytes: 0,
            fuel: 0,
            cancellation_checks: 0,
            fuel_since_cancellation: 0,
        };
        work.check_cancellation()?;
        Ok(work)
    }

    pub(crate) const fn limits(&self) -> GeometryLimits {
        self.limits
    }

    pub(crate) const fn segments(&self) -> u64 {
        self.segments
    }

    pub(crate) const fn edges(&self) -> u64 {
        self.edges
    }

    pub(crate) const fn samples(&self) -> u64 {
        self.samples
    }

    pub(crate) const fn dash_chunks(&self) -> u64 {
        self.dash_chunks
    }

    pub(crate) const fn stroke_runs(&self) -> u64 {
        self.stroke_runs
    }

    pub(crate) const fn stroke_primitives(&self) -> u64 {
        self.stroke_primitives
    }

    pub(crate) const fn geometry_bytes(&self) -> u64 {
        self.geometry_bytes
    }

    pub(crate) const fn peak_geometry_bytes(&self) -> u64 {
        self.peak_geometry_bytes
    }

    pub(crate) fn tighten_geometry_bytes_limit(
        &mut self,
        maximum: u64,
    ) -> Result<(), GeometryFailure> {
        if maximum == 0
            || maximum > self.limits.max_geometry_bytes
            || self.peak_geometry_bytes > maximum
        {
            return Err(GeometryFailure::InvalidGeometry);
        }
        self.limits.max_geometry_bytes = maximum;
        Ok(())
    }

    pub(crate) const fn fuel(&self) -> u64 {
        self.fuel
    }

    pub(crate) const fn cancellation_checks(&self) -> u64 {
        self.cancellation_checks
    }

    pub(crate) fn charge_segments(&mut self, amount: u64) -> Result<(), GeometryFailure> {
        let next = checked_counter(
            self.segments,
            self.limits.max_segments,
            amount,
            GeometryLimitKind::Segments,
        )?;
        checked_counter(
            self.fuel,
            self.limits.max_fuel,
            amount,
            GeometryLimitKind::Fuel,
        )?;
        self.segments = next;
        self.charge_fuel(amount)
    }

    pub(crate) fn charge_edges(&mut self, amount: u64) -> Result<(), GeometryFailure> {
        let next = checked_counter(
            self.edges,
            self.limits.max_edges,
            amount,
            GeometryLimitKind::Edges,
        )?;
        checked_counter(
            self.fuel,
            self.limits.max_fuel,
            amount,
            GeometryLimitKind::Fuel,
        )?;
        self.edges = next;
        self.charge_fuel(amount)
    }

    pub(crate) fn charge_samples(&mut self, amount: u64) -> Result<(), GeometryFailure> {
        let next = self.preflight_samples(amount)?;
        self.samples = next;
        self.charge_fuel(amount)
    }

    pub(crate) fn preflight_samples(&self, amount: u64) -> Result<u64, GeometryFailure> {
        let next = checked_counter(
            self.samples,
            self.limits.max_samples,
            amount,
            GeometryLimitKind::Samples,
        )?;
        self.preflight_fuel(amount)?;
        Ok(next)
    }

    pub(crate) fn preflight_fuel(&self, amount: u64) -> Result<u64, GeometryFailure> {
        checked_counter(
            self.fuel,
            self.limits.max_fuel,
            amount,
            GeometryLimitKind::Fuel,
        )
    }

    pub(crate) fn charge_dash_chunks(&mut self, amount: u64) -> Result<(), GeometryFailure> {
        let next = checked_counter(
            self.dash_chunks,
            self.limits.max_dash_chunks,
            amount,
            GeometryLimitKind::DashChunks,
        )?;
        self.preflight_fuel(amount)?;
        self.dash_chunks = next;
        self.charge_fuel(amount)
    }

    pub(crate) fn charge_stroke_runs(&mut self, amount: u64) -> Result<(), GeometryFailure> {
        let next = self.preflight_stroke_runs(amount)?;
        self.stroke_runs = next;
        self.charge_fuel(amount)
    }

    pub(crate) fn preflight_stroke_runs(&self, amount: u64) -> Result<u64, GeometryFailure> {
        let next = checked_counter(
            self.stroke_runs,
            self.limits.max_stroke_runs,
            amount,
            GeometryLimitKind::StrokeRuns,
        )?;
        self.preflight_fuel(amount)?;
        Ok(next)
    }

    pub(crate) fn charge_stroke_primitives(&mut self, amount: u64) -> Result<(), GeometryFailure> {
        let next = checked_counter(
            self.stroke_primitives,
            self.limits.max_stroke_primitives,
            amount,
            GeometryLimitKind::StrokePrimitives,
        )?;
        self.preflight_fuel(amount)?;
        self.stroke_primitives = next;
        self.charge_fuel(amount)
    }

    pub(crate) fn try_reserve_geometry<T>(
        &mut self,
        values: &mut Vec<T>,
        additional: usize,
    ) -> Result<(), GeometryFailure> {
        self.try_reserve_geometry_with(
            values,
            additional,
            |replacement, target_capacity, target_bytes| {
                replacement.try_reserve_exact(target_capacity).map_err(|_| {
                    GeometryFailure::Allocation {
                        attempted_bytes: target_bytes,
                    }
                })
            },
        )
    }

    fn try_reserve_geometry_with<T>(
        &mut self,
        values: &mut Vec<T>,
        additional: usize,
        reserve: impl FnOnce(&mut Vec<T>, usize, u64) -> Result<(), GeometryFailure>,
    ) -> Result<(), GeometryFailure> {
        let item_size = u64::try_from(std::mem::size_of::<T>())
            .map_err(|_| GeometryFailure::NumericOverflow)?;
        let old_capacity = u64::try_from(values.capacity())
            .map_err(|_| GeometryFailure::NumericOverflow)?
            .checked_mul(item_size)
            .ok_or(GeometryFailure::NumericOverflow)?;
        let required_items = values
            .len()
            .checked_add(additional)
            .ok_or(GeometryFailure::NumericOverflow)?;
        let current_logical_capacity = logical_vector_capacity(values.len())?;
        let target_capacity = logical_vector_capacity(required_items)?;
        if values.capacity() < current_logical_capacity {
            return Err(GeometryFailure::InvalidGeometry);
        }
        if target_capacity == current_logical_capacity {
            return Ok(());
        }
        let target_bytes = u64::try_from(target_capacity)
            .map_err(|_| GeometryFailure::NumericOverflow)?
            .checked_mul(item_size)
            .ok_or(GeometryFailure::NumericOverflow)?;
        checked_counter(
            self.geometry_bytes,
            self.limits.max_geometry_bytes,
            target_bytes,
            GeometryLimitKind::GeometryBytes,
        )?;
        let move_fuel =
            u64::try_from(values.len()).map_err(|_| GeometryFailure::NumericOverflow)?;
        self.preflight_fuel(move_fuel)?;
        self.charge_fuel(move_fuel)?;
        self.check_cancellation()?;
        let mut replacement = Vec::new();
        reserve(&mut replacement, target_capacity, target_bytes)?;
        let new_capacity = u64::try_from(replacement.capacity())
            .map_err(|_| GeometryFailure::NumericOverflow)?
            .checked_mul(item_size)
            .ok_or(GeometryFailure::NumericOverflow)?;
        let transient_geometry_bytes = checked_counter(
            self.geometry_bytes,
            self.limits.max_geometry_bytes,
            new_capacity,
            GeometryLimitKind::GeometryBytes,
        )?;
        let retained_without_old = self
            .geometry_bytes
            .checked_sub(old_capacity)
            .ok_or(GeometryFailure::NumericOverflow)?;
        let committed_geometry_bytes = checked_counter(
            retained_without_old,
            self.limits.max_geometry_bytes,
            new_capacity,
            GeometryLimitKind::GeometryBytes,
        )?;
        replacement.append(values);
        *values = replacement;
        self.geometry_bytes = committed_geometry_bytes;
        self.peak_geometry_bytes = self.peak_geometry_bytes.max(transient_geometry_bytes);
        Ok(())
    }

    pub(crate) fn try_push_geometry<T>(
        &mut self,
        values: &mut Vec<T>,
        value: T,
    ) -> Result<(), GeometryFailure> {
        self.try_push_geometry_with(
            values,
            value,
            |replacement, target_capacity, target_bytes| {
                replacement.try_reserve_exact(target_capacity).map_err(|_| {
                    GeometryFailure::Allocation {
                        attempted_bytes: target_bytes,
                    }
                })
            },
        )
    }

    fn try_push_geometry_with<T>(
        &mut self,
        values: &mut Vec<T>,
        value: T,
        reserve: impl FnOnce(&mut Vec<T>, usize, u64) -> Result<(), GeometryFailure>,
    ) -> Result<(), GeometryFailure> {
        self.try_reserve_geometry_with(values, 1, reserve)?;
        values.push(value);
        Ok(())
    }

    pub(crate) fn charge_fuel(&mut self, amount: u64) -> Result<(), GeometryFailure> {
        self.fuel = checked_counter(
            self.fuel,
            self.limits.max_fuel,
            amount,
            GeometryLimitKind::Fuel,
        )?;
        self.fuel_since_cancellation = self
            .fuel_since_cancellation
            .checked_add(amount)
            .ok_or(GeometryFailure::NumericOverflow)?;
        while self.fuel_since_cancellation >= CANCELLATION_FUEL_INTERVAL {
            self.check_cancellation()?;
            self.fuel_since_cancellation -= CANCELLATION_FUEL_INTERVAL;
        }
        Ok(())
    }

    pub(crate) fn check_cancellation(&mut self) -> Result<(), GeometryFailure> {
        self.cancellation_checks = self
            .cancellation_checks
            .checked_add(1)
            .ok_or(GeometryFailure::NumericOverflow)?;
        if self.cancellation.is_cancelled() {
            return Err(GeometryFailure::Cancelled);
        }
        Ok(())
    }
}

pub(crate) fn logical_vector_capacity(length: usize) -> Result<usize, GeometryFailure> {
    if length == 0 {
        return Ok(0);
    }
    length
        .checked_next_power_of_two()
        .map(|capacity| capacity.max(4))
        .ok_or(GeometryFailure::NumericOverflow)
}

fn checked_counter(
    counter: u64,
    limit: u64,
    amount: u64,
    kind: GeometryLimitKind,
) -> Result<u64, GeometryFailure> {
    let next = counter
        .checked_add(amount)
        .ok_or(GeometryFailure::NumericOverflow)?;
    if next > limit {
        return Err(GeometryFailure::Limit {
            kind,
            limit,
            consumed: counter,
            attempted: amount,
        });
    }
    Ok(next)
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct Fixed(i64);

impl Fixed {
    pub(crate) const ZERO: Self = Self(0);
    pub(crate) const ONE: Self = Self(1_i64 << FIXED_FRACTION_BITS);
    pub(crate) const HALF: Self = Self(1_i64 << (FIXED_FRACTION_BITS - 1));

    pub(crate) const fn from_raw(raw: i64) -> Self {
        Self(raw)
    }

    pub(crate) const fn raw(self) -> i64 {
        self.0
    }

    pub(crate) fn from_i64(value: i64) -> Result<Self, GeometryFailure> {
        let raw = i128::from(value)
            .checked_mul(FIXED_ONE_I128)
            .ok_or(GeometryFailure::NumericOverflow)?;
        i64::try_from(raw)
            .map(Self)
            .map_err(|_| GeometryFailure::NumericOverflow)
    }

    pub(crate) fn from_scene(value: SceneScalar) -> Result<Self, GeometryFailure> {
        let numerator = i128::from(value.scaled())
            .checked_mul(FIXED_ONE_I128)
            .ok_or(GeometryFailure::NumericOverflow)?;
        rounded_divide(numerator, SCENE_SCALE_I128).map(Self)
    }

    pub(crate) fn checked_add(self, other: Self) -> Result<Self, GeometryFailure> {
        self.0
            .checked_add(other.0)
            .map(Self)
            .ok_or(GeometryFailure::NumericOverflow)
    }

    pub(crate) fn checked_sub(self, other: Self) -> Result<Self, GeometryFailure> {
        self.0
            .checked_sub(other.0)
            .map(Self)
            .ok_or(GeometryFailure::NumericOverflow)
    }

    pub(crate) fn checked_neg(self) -> Result<Self, GeometryFailure> {
        self.0
            .checked_neg()
            .map(Self)
            .ok_or(GeometryFailure::NumericOverflow)
    }

    pub(crate) fn checked_mul(self, other: Self) -> Result<Self, GeometryFailure> {
        let numerator = i128::from(self.0)
            .checked_mul(i128::from(other.0))
            .ok_or(GeometryFailure::NumericOverflow)?;
        rounded_divide(numerator, FIXED_ONE_I128).map(Self)
    }

    pub(crate) fn checked_div(self, other: Self) -> Result<Self, GeometryFailure> {
        if other == Self::ZERO {
            return Err(GeometryFailure::InvalidGeometry);
        }
        let numerator = i128::from(self.0)
            .checked_mul(FIXED_ONE_I128)
            .ok_or(GeometryFailure::NumericOverflow)?;
        rounded_divide(numerator, i128::from(other.0)).map(Self)
    }

    pub(crate) fn checked_mul_ratio(
        self,
        numerator: Self,
        denominator: Self,
    ) -> Result<Self, GeometryFailure> {
        if denominator == Self::ZERO {
            return Err(GeometryFailure::InvalidGeometry);
        }
        let product = i128::from(self.0)
            .checked_mul(i128::from(numerator.0))
            .ok_or(GeometryFailure::NumericOverflow)?;
        rounded_divide(product, i128::from(denominator.0)).map(Self)
    }

    pub(crate) fn checked_half(self) -> Result<Self, GeometryFailure> {
        rounded_divide(i128::from(self.0), 2).map(Self)
    }

    pub(crate) fn checked_average(self, other: Self) -> Result<Self, GeometryFailure> {
        let sum = i128::from(self.0)
            .checked_add(i128::from(other.0))
            .ok_or(GeometryFailure::NumericOverflow)?;
        rounded_divide(sum, 2).map(Self)
    }

    pub(crate) fn checked_lerp(
        self,
        other: Self,
        numerator: u64,
        denominator: u64,
    ) -> Result<Self, GeometryFailure> {
        if denominator == 0 || numerator > denominator {
            return Err(GeometryFailure::InvalidGeometry);
        }
        let delta = i128::from(other.0)
            .checked_sub(i128::from(self.0))
            .ok_or(GeometryFailure::NumericOverflow)?;
        let scaled = delta
            .checked_mul(i128::from(numerator))
            .ok_or(GeometryFailure::NumericOverflow)?;
        let offset = rounded_divide_i128(scaled, i128::from(denominator))?;
        let value = i128::from(self.0)
            .checked_add(offset)
            .ok_or(GeometryFailure::NumericOverflow)?;
        i64::try_from(value)
            .map(Self)
            .map_err(|_| GeometryFailure::NumericOverflow)
    }
}

fn rounded_divide(numerator: i128, denominator: i128) -> Result<i64, GeometryFailure> {
    i64::try_from(rounded_divide_i128(numerator, denominator)?)
        .map_err(|_| GeometryFailure::NumericOverflow)
}

pub(crate) fn rounded_divide_i128(
    numerator: i128,
    denominator: i128,
) -> Result<i128, GeometryFailure> {
    if denominator == 0 {
        return Err(GeometryFailure::InvalidGeometry);
    }
    let quotient = numerator
        .checked_div(denominator)
        .ok_or(GeometryFailure::NumericOverflow)?;
    let remainder = numerator
        .checked_rem(denominator)
        .ok_or(GeometryFailure::NumericOverflow)?;
    let twice_remainder = remainder
        .unsigned_abs()
        .checked_mul(2)
        .ok_or(GeometryFailure::NumericOverflow)?;
    let denominator_magnitude = denominator.unsigned_abs();
    if twice_remainder >= denominator_magnitude {
        quotient
            .checked_add(if numerator.is_negative() == denominator.is_negative() {
                1
            } else {
                -1
            })
            .ok_or(GeometryFailure::NumericOverflow)
    } else {
        Ok(quotient)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub(crate) struct FixedPoint {
    pub(crate) x: Fixed,
    pub(crate) y: Fixed,
}

impl FixedPoint {
    pub(crate) const fn new(x: Fixed, y: Fixed) -> Self {
        Self { x, y }
    }

    pub(crate) fn from_scene(point: ScenePoint) -> Result<Self, GeometryFailure> {
        Ok(Self::new(
            Fixed::from_scene(point.x())?,
            Fixed::from_scene(point.y())?,
        ))
    }

    pub(crate) fn checked_average(self, other: Self) -> Result<Self, GeometryFailure> {
        Ok(Self::new(
            self.x.checked_average(other.x)?,
            self.y.checked_average(other.y)?,
        ))
    }

    pub(crate) fn checked_lerp(
        self,
        other: Self,
        numerator: u64,
        denominator: u64,
    ) -> Result<Self, GeometryFailure> {
        Ok(Self::new(
            self.x.checked_lerp(other.x, numerator, denominator)?,
            self.y.checked_lerp(other.y, numerator, denominator)?,
        ))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Affine {
    a: Fixed,
    b: Fixed,
    c: Fixed,
    d: Fixed,
    e: Fixed,
    f: Fixed,
}

impl Affine {
    pub(crate) const IDENTITY: Self = Self {
        a: Fixed::ONE,
        b: Fixed::ZERO,
        c: Fixed::ZERO,
        d: Fixed::ONE,
        e: Fixed::ZERO,
        f: Fixed::ZERO,
    };

    pub(crate) const fn new(a: Fixed, b: Fixed, c: Fixed, d: Fixed, e: Fixed, f: Fixed) -> Self {
        Self { a, b, c, d, e, f }
    }

    pub(crate) fn from_scene(matrix: Matrix) -> Result<Self, GeometryFailure> {
        let [a, b, c, d, e, f] = matrix.components();
        Ok(Self::new(
            Fixed::from_scene(a)?,
            Fixed::from_scene(b)?,
            Fixed::from_scene(c)?,
            Fixed::from_scene(d)?,
            Fixed::from_scene(e)?,
            Fixed::from_scene(f)?,
        ))
    }

    pub(crate) fn apply(self, point: FixedPoint) -> Result<FixedPoint, GeometryFailure> {
        let x = fixed_sum(&[(self.a, point.x), (self.c, point.y)], self.e)?;
        let y = fixed_sum(&[(self.b, point.x), (self.d, point.y)], self.f)?;
        Ok(FixedPoint::new(x, y))
    }

    pub(crate) fn checked_concat(self, other: Self) -> Result<Self, GeometryFailure> {
        Ok(Self::new(
            fixed_sum(&[(self.a, other.a), (self.c, other.b)], Fixed::ZERO)?,
            fixed_sum(&[(self.b, other.a), (self.d, other.b)], Fixed::ZERO)?,
            fixed_sum(&[(self.a, other.c), (self.c, other.d)], Fixed::ZERO)?,
            fixed_sum(&[(self.b, other.c), (self.d, other.d)], Fixed::ZERO)?,
            fixed_sum(&[(self.a, other.e), (self.c, other.f)], self.e)?,
            fixed_sum(&[(self.b, other.e), (self.d, other.f)], self.f)?,
        ))
    }

    pub(crate) fn inverse(self) -> Result<Option<Self>, GeometryFailure> {
        let determinant = i128::from(self.a.raw())
            .checked_mul(i128::from(self.d.raw()))
            .and_then(|value| {
                i128::from(self.b.raw())
                    .checked_mul(i128::from(self.c.raw()))
                    .and_then(|other| value.checked_sub(other))
            })
            .ok_or(GeometryFailure::NumericOverflow)?;
        if determinant == 0 {
            return Ok(None);
        }
        let scale_squared = FIXED_ONE_I128
            .checked_mul(FIXED_ONE_I128)
            .ok_or(GeometryFailure::NumericOverflow)?;
        let coefficient = |value: i64, negate: bool| -> Result<Fixed, GeometryFailure> {
            let signed = if negate {
                i128::from(value)
                    .checked_neg()
                    .ok_or(GeometryFailure::NumericOverflow)?
            } else {
                i128::from(value)
            };
            let numerator = signed
                .checked_mul(scale_squared)
                .ok_or(GeometryFailure::NumericOverflow)?;
            rounded_divide(numerator, determinant).map(Fixed)
        };
        let a = coefficient(self.d.raw(), false)?;
        let b = coefficient(self.b.raw(), true)?;
        let c = coefficient(self.c.raw(), true)?;
        let d = coefficient(self.a.raw(), false)?;
        let e = fixed_sum(&[(a, self.e), (c, self.f)], Fixed::ZERO)?.checked_neg()?;
        let f = fixed_sum(&[(b, self.e), (d, self.f)], Fixed::ZERO)?.checked_neg()?;
        Ok(Some(Self::new(a, b, c, d, e, f)))
    }
}

fn fixed_sum(products: &[(Fixed, Fixed)], addend: Fixed) -> Result<Fixed, GeometryFailure> {
    let mut numerator = i128::from(addend.raw())
        .checked_mul(FIXED_ONE_I128)
        .ok_or(GeometryFailure::NumericOverflow)?;
    for (left, right) in products {
        numerator = numerator
            .checked_add(
                i128::from(left.raw())
                    .checked_mul(i128::from(right.raw()))
                    .ok_or(GeometryFailure::NumericOverflow)?,
            )
            .ok_or(GeometryFailure::NumericOverflow)?;
    }
    rounded_divide(numerator, FIXED_ONE_I128).map(Fixed)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PageDeviceMap {
    affine: Affine,
}

impl PageDeviceMap {
    pub(crate) fn new(
        geometry: PageGeometry,
        width: u32,
        height: u32,
    ) -> Result<Self, GeometryFailure> {
        if width == 0 || height == 0 {
            return Err(GeometryFailure::InvalidGeometry);
        }
        let [left, bottom, right, top] = geometry.crop_box().coordinates();
        let left = Fixed::from_scene(left)?;
        let bottom = Fixed::from_scene(bottom)?;
        let right = Fixed::from_scene(right)?;
        let top = Fixed::from_scene(top)?;
        let crop_width = right.checked_sub(left)?;
        let crop_height = top.checked_sub(bottom)?;
        if crop_width <= Fixed::ZERO || crop_height <= Fixed::ZERO {
            return Err(GeometryFailure::InvalidGeometry);
        }
        let width = Fixed::from_i64(i64::from(width))?;
        let height = Fixed::from_i64(i64::from(height))?;
        let width_over_crop_width = width.checked_div(crop_width)?;
        let width_over_crop_height = width.checked_div(crop_height)?;
        let height_over_crop_width = height.checked_div(crop_width)?;
        let height_over_crop_height = height.checked_div(crop_height)?;

        let affine = match geometry.rotation() {
            PageRotation::Degrees0 => Affine::new(
                width_over_crop_width,
                Fixed::ZERO,
                Fixed::ZERO,
                height_over_crop_height.checked_neg()?,
                left.checked_mul(width_over_crop_width)?.checked_neg()?,
                top.checked_mul(height_over_crop_height)?,
            ),
            PageRotation::Degrees90 => Affine::new(
                Fixed::ZERO,
                height_over_crop_width,
                width_over_crop_height,
                Fixed::ZERO,
                bottom.checked_mul(width_over_crop_height)?.checked_neg()?,
                left.checked_mul(height_over_crop_width)?.checked_neg()?,
            ),
            PageRotation::Degrees180 => Affine::new(
                width_over_crop_width.checked_neg()?,
                Fixed::ZERO,
                Fixed::ZERO,
                height_over_crop_height,
                right.checked_mul(width_over_crop_width)?,
                bottom.checked_mul(height_over_crop_height)?.checked_neg()?,
            ),
            PageRotation::Degrees270 => Affine::new(
                Fixed::ZERO,
                height_over_crop_width.checked_neg()?,
                width_over_crop_height.checked_neg()?,
                Fixed::ZERO,
                top.checked_mul(width_over_crop_height)?,
                right.checked_mul(height_over_crop_width)?,
            ),
        };
        Ok(Self { affine })
    }

    pub(crate) const fn affine(self) -> Affine {
        self.affine
    }

    pub(crate) fn combined(self, transform: Matrix) -> Result<Affine, GeometryFailure> {
        self.affine.checked_concat(Affine::from_scene(transform)?)
    }

    pub(crate) fn map_page_point(self, point: ScenePoint) -> Result<FixedPoint, GeometryFailure> {
        self.affine.apply(FixedPoint::from_scene(point)?)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FlattenedSubpath {
    points: Vec<FixedPoint>,
    closed: bool,
    has_segments: bool,
}

impl FlattenedSubpath {
    pub(crate) fn points(&self) -> &[FixedPoint] {
        &self.points
    }

    pub(crate) const fn closed(&self) -> bool {
        self.closed
    }

    pub(crate) const fn has_segments(&self) -> bool {
        self.has_segments
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FlattenedPath {
    subpaths: Vec<FlattenedSubpath>,
}

impl FlattenedPath {
    pub(crate) fn subpaths(&self) -> &[FlattenedSubpath] {
        &self.subpaths
    }
}

#[derive(Clone, Copy)]
struct CubicPair {
    output: [FixedPoint; 4],
    measure: [FixedPoint; 4],
}

pub(crate) fn flatten_path(
    path: &PathResource,
    output_transform: Affine,
    flatness_transform: Affine,
    tolerance: Fixed,
    max_recursion: u8,
    work: &mut GeometryWork<'_>,
) -> Result<FlattenedPath, GeometryFailure> {
    if tolerance <= Fixed::ZERO || max_recursion == 0 {
        return Err(GeometryFailure::InvalidGeometry);
    }
    let mut subpaths = Vec::new();
    let mut current_output = None;
    let mut current_measure = None;
    let mut active: Option<FlattenedSubpath> = None;

    for segment in path.segments() {
        work.charge_fuel(1)?;
        match *segment {
            PathSegment::MoveTo(point) => {
                finish_open_subpath(&mut subpaths, active.take(), work)?;
                let source = FixedPoint::from_scene(point)?;
                let output = output_transform.apply(source)?;
                let measure = flatness_transform.apply(source)?;
                let mut points = Vec::new();
                work.try_push_geometry(&mut points, output)?;
                active = Some(FlattenedSubpath {
                    points,
                    closed: false,
                    has_segments: false,
                });
                current_output = Some(output);
                current_measure = Some(measure);
            }
            PathSegment::LineTo(point) => {
                let source = FixedPoint::from_scene(point)?;
                let output = output_transform.apply(source)?;
                let measure = flatness_transform.apply(source)?;
                let subpath = active.as_mut().ok_or(GeometryFailure::InvalidGeometry)?;
                work.charge_segments(1)?;
                work.try_push_geometry(&mut subpath.points, output)?;
                subpath.has_segments = true;
                current_output = Some(output);
                current_measure = Some(measure);
            }
            PathSegment::CubicTo {
                control_1,
                control_2,
                end,
            } => {
                let start_output = current_output.ok_or(GeometryFailure::InvalidGeometry)?;
                let start_measure = current_measure.ok_or(GeometryFailure::InvalidGeometry)?;
                let source_control_1 = FixedPoint::from_scene(control_1)?;
                let source_control_2 = FixedPoint::from_scene(control_2)?;
                let source_end = FixedPoint::from_scene(end)?;
                let pair = CubicPair {
                    output: [
                        start_output,
                        output_transform.apply(source_control_1)?,
                        output_transform.apply(source_control_2)?,
                        output_transform.apply(source_end)?,
                    ],
                    measure: [
                        start_measure,
                        flatness_transform.apply(source_control_1)?,
                        flatness_transform.apply(source_control_2)?,
                        flatness_transform.apply(source_end)?,
                    ],
                };
                let subpath = active.as_mut().ok_or(GeometryFailure::InvalidGeometry)?;
                flatten_cubic(pair, tolerance, max_recursion, 0, &mut subpath.points, work)?;
                subpath.has_segments = true;
                current_output = Some(pair.output[3]);
                current_measure = Some(pair.measure[3]);
            }
            PathSegment::ClosePath => {
                let mut subpath = active.take().ok_or(GeometryFailure::InvalidGeometry)?;
                subpath.closed = true;
                work.try_push_geometry(&mut subpaths, subpath)?;
                current_output = None;
                current_measure = None;
            }
        }
    }
    finish_open_subpath(&mut subpaths, active, work)?;
    Ok(FlattenedPath { subpaths })
}

fn finish_open_subpath(
    subpaths: &mut Vec<FlattenedSubpath>,
    active: Option<FlattenedSubpath>,
    work: &mut GeometryWork<'_>,
) -> Result<(), GeometryFailure> {
    if let Some(subpath) = active {
        work.try_push_geometry(subpaths, subpath)?;
    }
    Ok(())
}

fn flatten_cubic(
    cubic: CubicPair,
    tolerance: Fixed,
    max_recursion: u8,
    depth: u8,
    output: &mut Vec<FixedPoint>,
    work: &mut GeometryWork<'_>,
) -> Result<(), GeometryFailure> {
    work.charge_fuel(1)?;
    if cubic_is_flat(cubic.measure, tolerance)? {
        work.charge_segments(1)?;
        work.try_push_geometry(output, cubic.output[3])?;
        return Ok(());
    }
    if depth >= max_recursion {
        return Err(GeometryFailure::Limit {
            kind: GeometryLimitKind::CurveRecursion,
            limit: u64::from(max_recursion),
            consumed: u64::from(depth),
            attempted: 1,
        });
    }
    let (left_output, right_output) = split_cubic(cubic.output)?;
    let (left_measure, right_measure) = split_cubic(cubic.measure)?;
    flatten_cubic(
        CubicPair {
            output: left_output,
            measure: left_measure,
        },
        tolerance,
        max_recursion,
        depth + 1,
        output,
        work,
    )?;
    flatten_cubic(
        CubicPair {
            output: right_output,
            measure: right_measure,
        },
        tolerance,
        max_recursion,
        depth + 1,
        output,
        work,
    )
}

fn split_cubic(
    points: [FixedPoint; 4],
) -> Result<([FixedPoint; 4], [FixedPoint; 4]), GeometryFailure> {
    let p01 = points[0].checked_average(points[1])?;
    let p12 = points[1].checked_average(points[2])?;
    let p23 = points[2].checked_average(points[3])?;
    let p012 = p01.checked_average(p12)?;
    let p123 = p12.checked_average(p23)?;
    let midpoint = p012.checked_average(p123)?;
    Ok((
        [points[0], p01, p012, midpoint],
        [midpoint, p123, p23, points[3]],
    ))
}

fn cubic_is_flat(points: [FixedPoint; 4], tolerance: Fixed) -> Result<bool, GeometryFailure> {
    let chord_x = i128::from(points[3].x.raw())
        .checked_sub(i128::from(points[0].x.raw()))
        .ok_or(GeometryFailure::NumericOverflow)?;
    let chord_y = i128::from(points[3].y.raw())
        .checked_sub(i128::from(points[0].y.raw()))
        .ok_or(GeometryFailure::NumericOverflow)?;
    let chord_length_squared = squared_sum(chord_x, chord_y)?;
    let tolerance_squared = i128::from(tolerance.raw())
        .checked_mul(i128::from(tolerance.raw()))
        .ok_or(GeometryFailure::NumericOverflow)?;
    if chord_length_squared == 0 {
        return Ok(
            point_distance_squared(points[0], points[1])? <= tolerance_squared
                && point_distance_squared(points[0], points[2])? <= tolerance_squared,
        );
    }
    let chord_length = integer_sqrt(
        u128::try_from(chord_length_squared).map_err(|_| GeometryFailure::NumericOverflow)?,
    );
    let chord_length =
        i128::try_from(chord_length).map_err(|_| GeometryFailure::NumericOverflow)?;
    let threshold = i128::from(tolerance.raw())
        .checked_mul(chord_length)
        .ok_or(GeometryFailure::NumericOverflow)?;
    let chord_length_squared_with_tolerance = chord_length_squared
        .checked_add(threshold)
        .ok_or(GeometryFailure::NumericOverflow)?;
    for control in [points[1], points[2]] {
        let relative_x = i128::from(control.x.raw())
            .checked_sub(i128::from(points[0].x.raw()))
            .ok_or(GeometryFailure::NumericOverflow)?;
        let relative_y = i128::from(control.y.raw())
            .checked_sub(i128::from(points[0].y.raw()))
            .ok_or(GeometryFailure::NumericOverflow)?;
        let cross = chord_x
            .checked_mul(relative_y)
            .and_then(|left| {
                chord_y
                    .checked_mul(relative_x)
                    .and_then(|right| left.checked_sub(right))
            })
            .ok_or(GeometryFailure::NumericOverflow)?;
        if cross.unsigned_abs() > threshold.unsigned_abs() {
            return Ok(false);
        }
        let projection = chord_x
            .checked_mul(relative_x)
            .and_then(|left| {
                chord_y
                    .checked_mul(relative_y)
                    .and_then(|right| left.checked_add(right))
            })
            .ok_or(GeometryFailure::NumericOverflow)?;
        if projection < -threshold || projection > chord_length_squared_with_tolerance {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(crate) fn point_distance_squared(
    first: FixedPoint,
    second: FixedPoint,
) -> Result<i128, GeometryFailure> {
    let x = i128::from(first.x.raw())
        .checked_sub(i128::from(second.x.raw()))
        .ok_or(GeometryFailure::NumericOverflow)?;
    let y = i128::from(first.y.raw())
        .checked_sub(i128::from(second.y.raw()))
        .ok_or(GeometryFailure::NumericOverflow)?;
    squared_sum(x, y)
}

fn squared_sum(x: i128, y: i128) -> Result<i128, GeometryFailure> {
    x.checked_mul(x)
        .and_then(|x_squared| {
            y.checked_mul(y)
                .and_then(|y_squared| x_squared.checked_add(y_squared))
        })
        .ok_or(GeometryFailure::NumericOverflow)
}

pub(crate) fn integer_sqrt(value: u128) -> u128 {
    if value < 2 {
        return value;
    }
    let bit_width = 128 - value.leading_zeros();
    let mut current = 1_u128 << bit_width.div_ceil(2);
    loop {
        let next = (current + value / current) / 2;
        if next >= current {
            return current;
        }
        current = next;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use pdf_rs_scene::{
        Matrix, PageGeometry, PageRotation, PathResource, PathSegment, ScenePoint, SceneRect,
        SceneScalar,
    };

    use super::{
        Affine, Fixed, FixedPoint, GeometryCancellation, GeometryFailure, GeometryLimitKind,
        GeometryLimits, GeometryWork, PageDeviceMap, flatten_path, integer_sqrt,
        logical_vector_capacity, rounded_divide_i128,
    };

    fn scalar(value: &str) -> SceneScalar {
        SceneScalar::from_decimal(value).unwrap()
    }

    fn point(x: &str, y: &str) -> ScenePoint {
        ScenePoint::new(scalar(x), scalar(y))
    }

    fn geometry(rotation: PageRotation) -> PageGeometry {
        let bounds =
            SceneRect::new([scalar("10"), scalar("20"), scalar("110"), scalar("220")]).unwrap();
        PageGeometry::new(bounds, bounds, rotation)
    }

    struct Cancellation {
        cancel_at: u64,
        calls: AtomicU64,
    }

    impl Cancellation {
        fn never() -> Self {
            Self {
                cancel_at: u64::MAX,
                calls: AtomicU64::new(0),
            }
        }

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

    #[test]
    fn page_mapping_is_exact_for_all_canonical_rotations() {
        let cases = [
            (
                PageRotation::Degrees0,
                [
                    (point("10", "20"), (0, 200)),
                    (point("110", "20"), (100, 200)),
                    (point("10", "220"), (0, 0)),
                    (point("110", "220"), (100, 0)),
                ],
            ),
            (
                PageRotation::Degrees90,
                [
                    (point("10", "20"), (0, 0)),
                    (point("110", "20"), (0, 200)),
                    (point("10", "220"), (100, 0)),
                    (point("110", "220"), (100, 200)),
                ],
            ),
            (
                PageRotation::Degrees180,
                [
                    (point("10", "20"), (100, 0)),
                    (point("110", "20"), (0, 0)),
                    (point("10", "220"), (100, 200)),
                    (point("110", "220"), (0, 200)),
                ],
            ),
            (
                PageRotation::Degrees270,
                [
                    (point("10", "20"), (100, 200)),
                    (point("110", "20"), (100, 0)),
                    (point("10", "220"), (0, 200)),
                    (point("110", "220"), (0, 0)),
                ],
            ),
        ];
        for (rotation, points) in cases {
            let map = PageDeviceMap::new(geometry(rotation), 100, 200).unwrap();
            for (source, expected) in points {
                let mapped = map.map_page_point(source).unwrap();
                assert_eq!(mapped.x, Fixed::from_i64(expected.0).unwrap());
                assert_eq!(mapped.y, Fixed::from_i64(expected.1).unwrap());
            }
        }
    }

    #[test]
    fn affine_inverse_round_trips_nonuniform_transform() {
        let affine = Affine::new(
            Fixed::from_i64(2).unwrap(),
            Fixed::from_raw(Fixed::ONE.raw() / 4),
            Fixed::from_raw(Fixed::ONE.raw() / 2),
            Fixed::from_i64(3).unwrap(),
            Fixed::from_i64(7).unwrap(),
            Fixed::from_i64(-11).unwrap(),
        );
        let point = FixedPoint::new(
            Fixed::from_raw(13 * Fixed::ONE.raw() / 8),
            Fixed::from_raw(-9 * Fixed::ONE.raw() / 16),
        );
        let transformed = affine.apply(point).unwrap();
        let inverse = affine.inverse().unwrap().unwrap();
        let restored = inverse.apply(transformed).unwrap();
        assert!((restored.x.raw() - point.x.raw()).abs() <= 2);
        assert!((restored.y.raw() - point.y.raw()).abs() <= 2);
        assert_eq!(
            Affine::new(
                Fixed::ONE,
                Fixed::ZERO,
                Fixed::from_i64(2).unwrap(),
                Fixed::ZERO,
                Fixed::ZERO,
                Fixed::ZERO,
            )
            .inverse()
            .unwrap(),
            None
        );
    }

    #[test]
    fn adaptive_de_casteljau_is_repeatable_and_bounded() {
        let path = PathResource::new(vec![
            PathSegment::MoveTo(point("0", "0")),
            PathSegment::CubicTo {
                control_1: point("0", "100"),
                control_2: point("100", "100"),
                end: point("100", "0"),
            },
        ])
        .unwrap();
        let cancellation = Cancellation::never();
        let mut first_work = GeometryWork::new(GeometryLimits::default(), &cancellation).unwrap();
        let first = flatten_path(
            &path,
            Affine::IDENTITY,
            Affine::IDENTITY,
            Fixed::from_raw(Fixed::ONE.raw() / 256),
            16,
            &mut first_work,
        )
        .unwrap();
        let mut second_work = GeometryWork::new(GeometryLimits::default(), &cancellation).unwrap();
        let second = flatten_path(
            &path,
            Affine::IDENTITY,
            Affine::IDENTITY,
            Fixed::from_raw(Fixed::ONE.raw() / 256),
            16,
            &mut second_work,
        )
        .unwrap();
        assert_eq!(first, second);
        assert!(first.subpaths()[0].points().len() > 2);
        assert_eq!(first_work.segments(), second_work.segments());
        assert!(first_work.fuel() < 1_000_000);
    }

    #[test]
    fn flattening_fails_closed_when_fixed_recursion_cannot_meet_tolerance() {
        let path = PathResource::new(vec![
            PathSegment::MoveTo(point("0", "0")),
            PathSegment::CubicTo {
                control_1: point("0", "100"),
                control_2: point("100", "100"),
                end: point("100", "0"),
            },
        ])
        .unwrap();
        let cancellation = Cancellation::never();
        let mut work = GeometryWork::new(GeometryLimits::default(), &cancellation).unwrap();
        assert!(matches!(
            flatten_path(
                &path,
                Affine::IDENTITY,
                Affine::IDENTITY,
                Fixed::from_raw(1),
                1,
                &mut work
            ),
            Err(GeometryFailure::Limit {
                kind: GeometryLimitKind::CurveRecursion,
                limit: 1,
                consumed: 1,
                attempted: 1
            })
        ));
    }

    #[test]
    fn collinear_cubic_overshoot_is_not_misclassified_as_a_short_line() {
        let path = PathResource::new(vec![
            PathSegment::MoveTo(point("0", "0")),
            PathSegment::CubicTo {
                control_1: point("100", "0"),
                control_2: point("-100", "0"),
                end: point("1", "0"),
            },
        ])
        .unwrap();
        let cancellation = Cancellation::never();
        let mut work = GeometryWork::new(GeometryLimits::default(), &cancellation).unwrap();
        let flattened = flatten_path(
            &path,
            Affine::IDENTITY,
            Affine::IDENTITY,
            Fixed::from_raw(Fixed::ONE.raw() / 256),
            16,
            &mut work,
        )
        .unwrap();
        let points = flattened.subpaths()[0].points();
        assert!(points.len() > 2);
        assert!(
            points
                .iter()
                .any(|point| point.x > Fixed::from_i64(20).unwrap())
        );
        assert!(
            points
                .iter()
                .any(|point| point.x < Fixed::from_i64(-20).unwrap())
        );
    }

    #[test]
    fn independent_one_less_segment_and_fuel_budgets_fail_before_commit() {
        let path = PathResource::new(vec![
            PathSegment::MoveTo(point("0", "0")),
            PathSegment::LineTo(point("1", "0")),
            PathSegment::LineTo(point("1", "1")),
        ])
        .unwrap();
        let cancellation = Cancellation::never();
        let mut work = GeometryWork::new(
            GeometryLimits {
                max_segments: 1,
                ..GeometryLimits::default()
            },
            &cancellation,
        )
        .unwrap();
        assert!(matches!(
            flatten_path(
                &path,
                Affine::IDENTITY,
                Affine::IDENTITY,
                Fixed::from_raw(Fixed::ONE.raw() / 256),
                16,
                &mut work
            ),
            Err(GeometryFailure::Limit {
                kind: GeometryLimitKind::Segments,
                limit: 1,
                consumed: 1,
                attempted: 1
            })
        ));
        assert_eq!(work.segments(), 1);

        let mut work = GeometryWork::new(
            GeometryLimits {
                max_fuel: 1,
                ..GeometryLimits::default()
            },
            &cancellation,
        )
        .unwrap();
        assert!(matches!(
            flatten_path(
                &path,
                Affine::IDENTITY,
                Affine::IDENTITY,
                Fixed::from_raw(Fixed::ONE.raw() / 256),
                16,
                &mut work
            ),
            Err(GeometryFailure::Limit {
                kind: GeometryLimitKind::Fuel,
                limit: 1,
                ..
            })
        ));
    }

    #[test]
    fn unknown_geometry_vectors_grow_geometrically_and_charge_move_work() {
        let cancellation = Cancellation::never();
        let mut work = GeometryWork::new(GeometryLimits::default(), &cancellation).unwrap();
        let mut values = Vec::new();
        for _ in 0..4 {
            work.try_push_geometry(&mut values, 0_u64).unwrap();
        }
        let fuel_before_growth = work.fuel();
        work.try_push_geometry(&mut values, 0_u64).unwrap();
        assert!(values.capacity() >= 8);
        assert_eq!(work.fuel() - fuel_before_growth, 4);
        assert_eq!(
            work.geometry_bytes(),
            u64::try_from(values.capacity()).unwrap() * 8
        );
    }

    #[test]
    fn geometry_growth_accounts_for_transient_replacement_capacity_transactionally() {
        let cancellation = Cancellation::never();
        let mut work = GeometryWork::new(GeometryLimits::default(), &cancellation).unwrap();
        let mut values = Vec::new();
        work.try_push_geometry(&mut values, 10_u64).unwrap();
        while values.len() < 4 {
            work.try_push_geometry(&mut values, 20_u64).unwrap();
        }
        let original = values.clone();
        let original_capacity = values.capacity();
        let original_geometry_bytes = work.geometry_bytes();
        let target_capacity = 8;
        let target_bytes = u64::try_from(target_capacity).unwrap() * 8;
        let exact_transient_bytes = original_geometry_bytes.checked_add(target_bytes).unwrap();
        work.limits.max_geometry_bytes = exact_transient_bytes;

        assert!(matches!(
            work.try_reserve_geometry_with(
                &mut values,
                1,
                |replacement, target_capacity, target_bytes| {
                    replacement
                        .try_reserve_exact(target_capacity.checked_add(1).unwrap())
                        .map_err(|_| GeometryFailure::Allocation {
                            attempted_bytes: target_bytes.checked_add(8).unwrap(),
                        })
                }
            ),
            Err(GeometryFailure::Limit {
                kind: GeometryLimitKind::GeometryBytes,
                limit,
                consumed,
                attempted,
                ..
            }) if limit == exact_transient_bytes
                && consumed == original_geometry_bytes
                && attempted > target_bytes
        ));
        assert_eq!(values, original);
        assert_eq!(values.capacity(), original_capacity);
        assert_eq!(work.geometry_bytes(), original_geometry_bytes);
        assert_eq!(work.peak_geometry_bytes(), original_geometry_bytes);

        work.limits.max_geometry_bytes = GeometryLimits::default().max_geometry_bytes;
        work.try_push_geometry(&mut values, 30_u64).unwrap();
        assert_eq!(values.len(), original.len() + 1);
        assert_eq!(
            work.geometry_bytes(),
            u64::try_from(values.capacity()).unwrap() * 8
        );
        assert_eq!(work.peak_geometry_bytes(), exact_transient_bytes);
    }

    #[test]
    fn one_less_transient_geometry_budget_rejects_growth_before_mutation() {
        let cancellation = Cancellation::never();
        let mut work = GeometryWork::new(GeometryLimits::default(), &cancellation).unwrap();
        let mut values = Vec::new();
        work.try_push_geometry(&mut values, 10_u64).unwrap();
        while values.len() < 4 {
            work.try_push_geometry(&mut values, 20_u64).unwrap();
        }
        let original = values.clone();
        let original_capacity = values.capacity();
        let original_geometry_bytes = work.geometry_bytes();
        let target_capacity = 8;
        let target_bytes = u64::try_from(target_capacity).unwrap() * 8;
        let limit = original_geometry_bytes
            .checked_add(target_bytes)
            .unwrap()
            .checked_sub(1)
            .unwrap();
        work.limits.max_geometry_bytes = limit;

        assert_eq!(
            work.try_push_geometry(&mut values, 30_u64),
            Err(GeometryFailure::Limit {
                kind: GeometryLimitKind::GeometryBytes,
                limit,
                consumed: original_geometry_bytes,
                attempted: target_bytes,
            })
        );
        assert_eq!(values, original);
        assert_eq!(values.capacity(), original_capacity);
        assert_eq!(work.geometry_bytes(), original_geometry_bytes);
        assert_eq!(work.peak_geometry_bytes(), original_geometry_bytes);
    }

    #[test]
    fn geometry_limit_can_only_tighten_without_hiding_an_observed_peak() {
        let cancellation = Cancellation::never();
        let mut work = GeometryWork::new(GeometryLimits::default(), &cancellation).unwrap();
        let mut values = Vec::new();
        work.try_push_geometry(&mut values, 10_u64).unwrap();
        let observed_peak = work.peak_geometry_bytes();
        assert!(observed_peak > 0);

        assert_eq!(
            work.tighten_geometry_bytes_limit(observed_peak - 1),
            Err(GeometryFailure::InvalidGeometry)
        );
        assert_eq!(
            work.tighten_geometry_bytes_limit(GeometryLimits::default().max_geometry_bytes + 1),
            Err(GeometryFailure::InvalidGeometry)
        );
        work.tighten_geometry_bytes_limit(observed_peak).unwrap();
        assert_eq!(work.limits().max_geometry_bytes, observed_peak);
    }

    #[test]
    fn dropped_geometry_capacity_remains_a_conservative_retained_upper_bound() {
        let cancellation = Cancellation::never();
        let mut work = GeometryWork::new(GeometryLimits::default(), &cancellation).unwrap();
        let charged = {
            let mut temporary = Vec::new();
            work.try_push_geometry(&mut temporary, 1_u64).unwrap();
            work.geometry_bytes()
        };
        assert!(charged > 0);
        assert_eq!(work.geometry_bytes(), charged);

        let next_target_bytes = u64::try_from(logical_vector_capacity(1).unwrap()).unwrap() * 8;
        work.limits.max_geometry_bytes = charged
            .checked_add(next_target_bytes)
            .unwrap()
            .checked_sub(1)
            .unwrap();
        let mut next = Vec::new();
        assert!(matches!(
            work.try_push_geometry(&mut next, 2_u64),
            Err(GeometryFailure::Limit {
                kind: GeometryLimitKind::GeometryBytes,
                consumed,
                ..
            }) if consumed == charged
        ));
        assert!(next.is_empty());
    }

    #[test]
    fn allocator_overcapacity_does_not_change_geometry_fuel_schedule() {
        fn run(
            extra_capacity: usize,
            max_fuel: u64,
        ) -> (Result<(), GeometryFailure>, Vec<u64>, u64, u64, u64) {
            let cancellation = Cancellation::never();
            let mut work = GeometryWork::new(
                GeometryLimits {
                    max_fuel,
                    ..GeometryLimits::default()
                },
                &cancellation,
            )
            .unwrap();
            let mut values = Vec::new();
            let mut result = Ok(());
            for value in 0..257 {
                result = work.try_push_geometry_with(
                    &mut values,
                    value,
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
                values,
                work.fuel(),
                work.cancellation_checks(),
                work.geometry_bytes(),
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
    fn geometry_growth_checks_cancellation_before_allocation() {
        let cancellation = Cancellation::at(2);
        let mut work = GeometryWork::new(GeometryLimits::default(), &cancellation).unwrap();
        let mut values = Vec::new();
        assert_eq!(
            work.try_push_geometry(&mut values, 1_u64),
            Err(GeometryFailure::Cancelled)
        );
        assert!(values.is_empty());
        assert_eq!(values.capacity(), 0);
        assert_eq!(work.geometry_bytes(), 0);
    }

    #[test]
    fn cancellation_is_probed_at_fixed_fuel_boundaries() {
        let cancellation = Cancellation::at(2);
        let mut work = GeometryWork::new(GeometryLimits::default(), &cancellation).unwrap();
        assert_eq!(work.charge_fuel(255), Ok(()));
        assert_eq!(cancellation.calls.load(Ordering::SeqCst), 1);
        assert_eq!(work.charge_fuel(1), Err(GeometryFailure::Cancelled));
        assert_eq!(work.cancellation_checks(), 2);
    }

    #[test]
    fn integer_square_root_is_floor_exact() {
        for value in 0_u128..10_000 {
            let root = integer_sqrt(value);
            assert!(root * root <= value);
            assert!((root + 1) * (root + 1) > value);
        }
    }

    #[test]
    fn scene_and_q32_matrix_paths_agree_at_representable_points() {
        let matrix = Matrix::new([
            scalar("2"),
            scalar("0.25"),
            scalar("0.5"),
            scalar("3"),
            scalar("7"),
            scalar("-11"),
        ]);
        let scene_point = point("1.5", "-0.5");
        let expected = matrix.checked_transform_point(scene_point).unwrap();
        let actual = Affine::from_scene(matrix)
            .unwrap()
            .apply(FixedPoint::from_scene(scene_point).unwrap())
            .unwrap();
        let expected = FixedPoint::from_scene(expected).unwrap();
        assert!((actual.x.raw() - expected.x.raw()).abs() <= 2);
        assert!((actual.y.raw() - expected.y.raw()).abs() <= 2);
    }

    #[test]
    fn signed_division_overflow_fails_closed() {
        for (numerator, denominator, expected) in [
            (1, 2, 1),
            (-1, 2, -1),
            (1, -2, -1),
            (-1, -2, 1),
            (3, 2, 2),
            (-3, 2, -2),
        ] {
            assert_eq!(
                rounded_divide_i128(numerator, denominator),
                Ok(expected),
                "ties round away from zero for {numerator}/{denominator}"
            );
        }
        assert_eq!(
            rounded_divide_i128(i128::MIN, -1),
            Err(GeometryFailure::NumericOverflow)
        );
    }
}
