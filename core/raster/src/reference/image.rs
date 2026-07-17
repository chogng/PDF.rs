use std::mem::size_of;

use pdf_rs_scene::{
    BlendMode, DeviceColor, ImageColorSpace, ImageResource, Matrix, PageGeometry, SceneUnit,
};

use super::coverage::{
    ClipStack, CoverageMask, SAMPLE_GRID_WIDTH, SAMPLES_PER_PIXEL, sample_point,
};
use super::geometry::{Fixed, GeometryFailure, GeometryLimitKind, PageDeviceMap};
use super::{
    NormalizedQ16, PremultipliedRgbaQ16, ReferenceBlendMode, ReferenceColorProfile,
    ReferenceSrgbQ16,
};

const CANCELLATION_FUEL_INTERVAL: u64 = 256;

#[allow(
    dead_code,
    reason = "the standalone image harness retains full-page output admission"
)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ImageLimitKind {
    SourcePixels,
    StrideBytes,
    DecodedBytes,
    OutputPixels,
    Samples,
    Conversions,
    RetainedBytes,
    Fuel,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ImageFailure {
    NumericOverflow,
    InvalidImage,
    UnsupportedInterpolation,
    Cancelled,
    Allocation {
        attempted_bytes: u64,
    },
    Limit {
        kind: ImageLimitKind,
        limit: u64,
        consumed: u64,
        attempted: u64,
    },
    GeometryLimit {
        kind: GeometryLimitKind,
        limit: u64,
        consumed: u64,
        attempted: u64,
    },
}

impl From<GeometryFailure> for ImageFailure {
    fn from(value: GeometryFailure) -> Self {
        match value {
            GeometryFailure::NumericOverflow => Self::NumericOverflow,
            GeometryFailure::InvalidGeometry => Self::InvalidImage,
            GeometryFailure::Cancelled => Self::Cancelled,
            GeometryFailure::Allocation { attempted_bytes } => Self::Allocation { attempted_bytes },
            GeometryFailure::Limit {
                kind,
                limit,
                consumed,
                attempted,
            } => Self::GeometryLimit {
                kind,
                limit,
                consumed,
                attempted,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ImageLimits {
    /// Maximum decoded source pixels admitted for one image command.
    pub(crate) max_source_pixels: u64,
    /// Maximum decoded row stride admitted for one image command.
    pub(crate) max_stride_bytes: u64,
    /// Maximum decoded source bytes admitted for one image command.
    pub(crate) max_decoded_bytes: u64,
    /// Maximum output pixels copied into the private result.
    pub(crate) max_output_pixels: u64,
    /// Conservative full-output sample admission cap for a non-singular command.
    ///
    /// Admission intentionally does not depend on clip coverage or whether the transformed image
    /// intersects the page. Published statistics retain the exact work actually performed.
    pub(crate) max_samples: u64,
    /// Conservative full-output color-conversion admission cap for a non-singular command.
    ///
    /// This is checked before allocation even when the command is fully clipped or off-page.
    pub(crate) max_conversions: u64,
    /// Maximum allocator-reported bytes retained by the private output.
    pub(crate) max_retained_bytes: u64,
    /// Conservative full-output fuel admission cap for a non-singular command.
    ///
    /// The admitted amount includes output copying, all samples and conversions, and final pixel
    /// averaging. Exact statistics can be lower for clipped or off-page commands.
    pub(crate) max_fuel: u64,
}

impl Default for ImageLimits {
    fn default() -> Self {
        Self {
            max_source_pixels: 67_108_864,
            max_stride_bytes: 64 * 1024 * 1024,
            max_decoded_bytes: 256 * 1024 * 1024,
            max_output_pixels: 67_108_864,
            max_samples: 1_000_000_000,
            max_conversions: 1_000_000_000,
            max_retained_bytes: 1024 * 1024 * 1024,
            max_fuel: 1_000_000_000,
        }
    }
}

pub(crate) trait ImageCancellation {
    fn is_cancelled(&self) -> bool;
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ImageStats {
    source_pixels: u64,
    stride_bytes: u64,
    decoded_bytes: u64,
    output_pixels: u64,
    samples: u64,
    conversions: u64,
    retained_bytes: u64,
    fuel: u64,
    cancellation_checks: u64,
}

impl ImageStats {
    pub(crate) const fn source_pixels(self) -> u64 {
        self.source_pixels
    }

    pub(crate) const fn stride_bytes(self) -> u64 {
        self.stride_bytes
    }

    pub(crate) const fn decoded_bytes(self) -> u64 {
        self.decoded_bytes
    }

    #[allow(
        dead_code,
        reason = "the standalone image harness measures output admission"
    )]
    pub(crate) const fn output_pixels(self) -> u64 {
        self.output_pixels
    }

    pub(crate) const fn samples(self) -> u64 {
        self.samples
    }

    pub(crate) const fn conversions(self) -> u64 {
        self.conversions
    }

    #[allow(
        dead_code,
        reason = "the standalone image harness measures retained output"
    )]
    pub(crate) const fn retained_bytes(self) -> u64 {
        self.retained_bytes
    }

    pub(crate) const fn fuel(self) -> u64 {
        self.fuel
    }

    pub(crate) const fn cancellation_checks(self) -> u64 {
        self.cancellation_checks
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(
    dead_code,
    reason = "retained by the standalone analytic image harness"
)]
pub(crate) struct ImageRaster {
    width: u32,
    height: u32,
    pixels: Vec<PremultipliedRgbaQ16>,
    stats: ImageStats,
}

#[allow(
    dead_code,
    reason = "retained by the standalone analytic image harness"
)]
impl ImageRaster {
    pub(crate) const PROFILE: &'static str = "reference-image-v1";

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

    pub(crate) const fn stats(&self) -> ImageStats {
        self.stats
    }
}

struct ImageWork<'a> {
    limits: ImageLimits,
    cancellation: &'a dyn ImageCancellation,
    stats: &'a mut ImageStats,
    fuel_since_cancellation: u64,
}

impl<'a> ImageWork<'a> {
    fn new(
        limits: ImageLimits,
        cancellation: &'a dyn ImageCancellation,
        stats: &'a mut ImageStats,
    ) -> Result<Self, ImageFailure> {
        if limits.max_source_pixels == 0
            || limits.max_decoded_bytes == 0
            || limits.max_samples == 0
            || limits.max_conversions == 0
            || limits.max_fuel == 0
        {
            return Err(ImageFailure::InvalidImage);
        }
        Self::new_mounted(limits, cancellation, stats)
    }

    fn new_mounted(
        limits: ImageLimits,
        cancellation: &'a dyn ImageCancellation,
        stats: &'a mut ImageStats,
    ) -> Result<Self, ImageFailure> {
        if limits.max_stride_bytes == 0
            || limits.max_output_pixels == 0
            || limits.max_retained_bytes == 0
        {
            return Err(ImageFailure::InvalidImage);
        }
        let mut work = Self {
            limits,
            cancellation,
            stats,
            fuel_since_cancellation: 0,
        };
        work.check_cancellation()?;
        Ok(work)
    }

    fn ensure(
        &self,
        kind: ImageLimitKind,
        limit: u64,
        consumed: u64,
        attempted: u64,
    ) -> Result<u64, ImageFailure> {
        let next = consumed
            .checked_add(attempted)
            .ok_or(ImageFailure::NumericOverflow)?;
        if next > limit {
            return Err(ImageFailure::Limit {
                kind,
                limit,
                consumed,
                attempted,
            });
        }
        Ok(next)
    }

    fn charge_fuel(&mut self, amount: u64) -> Result<(), ImageFailure> {
        self.stats.fuel = self.ensure(
            ImageLimitKind::Fuel,
            self.limits.max_fuel,
            self.stats.fuel,
            amount,
        )?;
        self.fuel_since_cancellation = self
            .fuel_since_cancellation
            .checked_add(amount)
            .ok_or(ImageFailure::NumericOverflow)?;
        while self.fuel_since_cancellation >= CANCELLATION_FUEL_INTERVAL {
            self.check_cancellation()?;
            self.fuel_since_cancellation -= CANCELLATION_FUEL_INTERVAL;
        }
        Ok(())
    }

    fn guard_sample(&mut self, convert: bool) -> Result<(u64, u64), ImageFailure> {
        let samples = self.ensure(
            ImageLimitKind::Samples,
            self.limits.max_samples,
            self.stats.samples,
            1,
        )?;
        let conversions = if convert {
            self.ensure(
                ImageLimitKind::Conversions,
                self.limits.max_conversions,
                self.stats.conversions,
                1,
            )?
        } else {
            self.stats.conversions
        };
        self.charge_fuel(1 + u64::from(convert))?;
        Ok((samples, conversions))
    }

    fn commit_sample(&mut self, samples: u64, conversions: u64) {
        self.stats.samples = samples;
        self.stats.conversions = conversions;
    }

    fn check_cancellation(&mut self) -> Result<(), ImageFailure> {
        self.stats.cancellation_checks = self
            .stats
            .cancellation_checks
            .checked_add(1)
            .ok_or(ImageFailure::NumericOverflow)?;
        if self.cancellation.is_cancelled() {
            return Err(ImageFailure::Cancelled);
        }
        Ok(())
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "the staged image kernel keeps geometry, paint, backdrop, clip, limits, and cancellation explicit"
)]
#[allow(
    dead_code,
    reason = "retained by the standalone analytic image harness"
)]
pub(crate) fn rasterize_image(
    image: &ImageResource,
    geometry: PageGeometry,
    transform: Matrix,
    output_width: u32,
    output_height: u32,
    alpha: SceneUnit,
    blend_mode: BlendMode,
    backdrop: &[PremultipliedRgbaQ16],
    clip: Option<&CoverageMask>,
    limits: ImageLimits,
    cancellation: &dyn ImageCancellation,
) -> Result<ImageRaster, ImageFailure> {
    let mut stats = ImageStats::default();
    let mut work = ImageWork::new(limits, cancellation, &mut stats)?;
    if image.bits_per_component() != 8 {
        return Err(ImageFailure::InvalidImage);
    }
    if image.interpolate() {
        return Err(ImageFailure::UnsupportedInterpolation);
    }
    let components = u64::from(image.color_space().components());
    let source_pixels = u64::from(image.width())
        .checked_mul(u64::from(image.height()))
        .ok_or(ImageFailure::NumericOverflow)?;
    work.stats.source_pixels = work.ensure(
        ImageLimitKind::SourcePixels,
        limits.max_source_pixels,
        0,
        source_pixels,
    )?;
    let stride_bytes = u64::from(image.width())
        .checked_mul(components)
        .ok_or(ImageFailure::NumericOverflow)?;
    work.stats.stride_bytes = work.ensure(
        ImageLimitKind::StrideBytes,
        limits.max_stride_bytes,
        0,
        stride_bytes,
    )?;
    let decoded_bytes = stride_bytes
        .checked_mul(u64::from(image.height()))
        .ok_or(ImageFailure::NumericOverflow)?;
    if decoded_bytes != u64::try_from(image.decoded().len()).unwrap_or(u64::MAX) {
        return Err(ImageFailure::InvalidImage);
    }
    work.stats.decoded_bytes = work.ensure(
        ImageLimitKind::DecodedBytes,
        limits.max_decoded_bytes,
        0,
        decoded_bytes,
    )?;

    let output_pixels = u64::from(output_width)
        .checked_mul(u64::from(output_height))
        .ok_or(ImageFailure::NumericOverflow)?;
    work.stats.output_pixels = work.ensure(
        ImageLimitKind::OutputPixels,
        limits.max_output_pixels,
        0,
        output_pixels,
    )?;
    if output_pixels == 0 || u64::try_from(backdrop.len()).unwrap_or(u64::MAX) != output_pixels {
        return Err(ImageFailure::InvalidImage);
    }
    if let Some(clip) = clip
        && (clip.width() != output_width || clip.height() != output_height)
    {
        return Err(ImageFailure::InvalidImage);
    }

    let device_to_image = PageDeviceMap::new(geometry, output_width, output_height)?
        .combined(transform)?
        .inverse()?;
    let admitted_samples = if device_to_image.is_some() {
        output_pixels
            .checked_mul(u64::from(SAMPLES_PER_PIXEL))
            .ok_or(ImageFailure::NumericOverflow)?
    } else {
        0
    };
    work.ensure(
        ImageLimitKind::Samples,
        limits.max_samples,
        0,
        admitted_samples,
    )?;
    work.ensure(
        ImageLimitKind::Conversions,
        limits.max_conversions,
        0,
        admitted_samples,
    )?;
    let admitted_sample_fuel = admitted_samples
        .checked_mul(2)
        .ok_or(ImageFailure::NumericOverflow)?;
    let admitted_pixel_fuel = if device_to_image.is_some() {
        output_pixels
    } else {
        0
    };
    let worst_case_fuel = output_pixels
        .checked_add(admitted_pixel_fuel)
        .and_then(|value| value.checked_add(admitted_sample_fuel))
        .ok_or(ImageFailure::NumericOverflow)?;
    work.ensure(ImageLimitKind::Fuel, limits.max_fuel, 0, worst_case_fuel)?;

    let retained_bytes = output_pixels
        .checked_mul(u64::try_from(size_of::<PremultipliedRgbaQ16>()).unwrap_or(u64::MAX))
        .ok_or(ImageFailure::NumericOverflow)?;
    work.ensure(
        ImageLimitKind::RetainedBytes,
        limits.max_retained_bytes,
        0,
        retained_bytes,
    )?;
    let capacity = usize::try_from(output_pixels).map_err(|_| ImageFailure::NumericOverflow)?;
    work.check_cancellation()?;
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(capacity)
        .map_err(|_| ImageFailure::Allocation {
            attempted_bytes: retained_bytes,
        })?;
    let actual_retained = u64::try_from(pixels.capacity())
        .map_err(|_| ImageFailure::NumericOverflow)?
        .checked_mul(u64::try_from(size_of::<PremultipliedRgbaQ16>()).unwrap_or(u64::MAX))
        .ok_or(ImageFailure::NumericOverflow)?;
    work.stats.retained_bytes = work.ensure(
        ImageLimitKind::RetainedBytes,
        limits.max_retained_bytes,
        0,
        actual_retained,
    )?;
    for pixel in backdrop {
        work.charge_fuel(1)?;
        pixels.push(*pixel);
    }

    let Some(device_to_image) = device_to_image else {
        // A rank-zero or rank-one image transform paints no area. It is a valid no-op, not a
        // page-level failure, and still publishes a private copy of the exact backdrop.
        work.check_cancellation()?;
        let stats = *work.stats;
        return Ok(ImageRaster {
            width: output_width,
            height: output_height,
            pixels,
            stats,
        });
    };
    let alpha = NormalizedQ16::from(alpha);
    let blend_mode = ReferenceBlendMode::from(blend_mode);
    for y in 0..output_height {
        for x in 0..output_width {
            let index =
                pixel_index(output_width, output_height, x, y).ok_or(ImageFailure::InvalidImage)?;
            let backdrop_pixel = backdrop[index];
            let clip_mask = clip
                .and_then(|mask| mask.sample_mask(x, y))
                .unwrap_or(u64::MAX);
            let mut sums = [0_u64; 4];
            for sample_y in 0..SAMPLE_GRID_WIDTH {
                for sample_x in 0..SAMPLE_GRID_WIDTH {
                    let bit = sample_y
                        .checked_mul(SAMPLE_GRID_WIDTH)
                        .and_then(|value| value.checked_add(sample_x))
                        .ok_or(ImageFailure::NumericOverflow)?;
                    let selected = clip_mask & (1_u64 << bit) != 0;
                    let sample = sample_point(x, y, sample_x, sample_y)?;
                    let image_point = device_to_image.apply(sample)?;
                    let source_index = if selected {
                        image_sample_index(image, image_point.x, image_point.y)?
                    } else {
                        None
                    };
                    let completed = work.guard_sample(source_index.is_some())?;
                    let composed = match source_index {
                        Some(source_index) => {
                            let source =
                                source_pixel(image, source_index)?.with_constant_alpha(alpha);
                            blend_mode.source_over(source, backdrop_pixel)
                        }
                        None => backdrop_pixel,
                    };
                    sums[0] = sums[0]
                        .checked_add(u64::from(composed.red().bits()))
                        .ok_or(ImageFailure::NumericOverflow)?;
                    sums[1] = sums[1]
                        .checked_add(u64::from(composed.green().bits()))
                        .ok_or(ImageFailure::NumericOverflow)?;
                    sums[2] = sums[2]
                        .checked_add(u64::from(composed.blue().bits()))
                        .ok_or(ImageFailure::NumericOverflow)?;
                    sums[3] = sums[3]
                        .checked_add(u64::from(composed.alpha().bits()))
                        .ok_or(ImageFailure::NumericOverflow)?;
                    work.commit_sample(completed.0, completed.1);
                }
            }
            let averaged = averaged_pixel(sums)?;
            work.charge_fuel(1)?;
            pixels[index] = averaged;
        }
    }
    work.check_cancellation()?;
    let stats = *work.stats;

    Ok(ImageRaster {
        width: output_width,
        height: output_height,
        pixels,
        stats,
    })
}

/// Paints one basic image directly into a job-private surface.
///
/// Unlike the staged standalone harness above, this mounted form never allocates or copies a
/// second full-page backdrop. A failure can leave the borrowed surface partially modified, but
/// the surface is owned exclusively by `ReferenceRenderJob` and is discarded on every failure.
#[allow(
    clippy::too_many_arguments,
    reason = "the mounted image kernel keeps geometry, paint, surface, clip, limits, and cancellation explicit"
)]
pub(crate) fn paint_image(
    image: &ImageResource,
    geometry: PageGeometry,
    transform: Matrix,
    output_width: u32,
    output_height: u32,
    alpha: SceneUnit,
    blend_mode: BlendMode,
    pixels: &mut [PremultipliedRgbaQ16],
    clip: Option<&ClipStack>,
    limits: ImageLimits,
    cancellation: &dyn ImageCancellation,
    progress: &mut ImageStats,
) -> Result<(), ImageFailure> {
    let mut work = ImageWork::new_mounted(limits, cancellation, progress)?;
    if image.bits_per_component() != 8 {
        return Err(ImageFailure::InvalidImage);
    }
    if image.interpolate() {
        return Err(ImageFailure::UnsupportedInterpolation);
    }
    let components = u64::from(image.color_space().components());
    let source_pixels = u64::from(image.width())
        .checked_mul(u64::from(image.height()))
        .ok_or(ImageFailure::NumericOverflow)?;
    work.stats.source_pixels = work.ensure(
        ImageLimitKind::SourcePixels,
        limits.max_source_pixels,
        0,
        source_pixels,
    )?;
    let stride_bytes = u64::from(image.width())
        .checked_mul(components)
        .ok_or(ImageFailure::NumericOverflow)?;
    work.stats.stride_bytes = work.ensure(
        ImageLimitKind::StrideBytes,
        limits.max_stride_bytes,
        0,
        stride_bytes,
    )?;
    let decoded_bytes = stride_bytes
        .checked_mul(u64::from(image.height()))
        .ok_or(ImageFailure::NumericOverflow)?;
    if decoded_bytes != u64::try_from(image.decoded().len()).unwrap_or(u64::MAX) {
        return Err(ImageFailure::InvalidImage);
    }
    work.stats.decoded_bytes = work.ensure(
        ImageLimitKind::DecodedBytes,
        limits.max_decoded_bytes,
        0,
        decoded_bytes,
    )?;

    let output_pixels = u64::from(output_width)
        .checked_mul(u64::from(output_height))
        .ok_or(ImageFailure::NumericOverflow)?;
    work.stats.output_pixels = work.ensure(
        ImageLimitKind::OutputPixels,
        limits.max_output_pixels,
        0,
        output_pixels,
    )?;
    if output_pixels == 0 || u64::try_from(pixels.len()).unwrap_or(u64::MAX) != output_pixels {
        return Err(ImageFailure::InvalidImage);
    }
    if let Some(clip) = clip
        && (clip.width() != output_width || clip.height() != output_height)
    {
        return Err(ImageFailure::InvalidImage);
    }

    let device_to_image = PageDeviceMap::new(geometry, output_width, output_height)?
        .combined(transform)?
        .inverse()?;
    let admitted_samples = if device_to_image.is_some() {
        output_pixels
            .checked_mul(u64::from(SAMPLES_PER_PIXEL))
            .ok_or(ImageFailure::NumericOverflow)?
    } else {
        0
    };
    work.ensure(
        ImageLimitKind::Samples,
        limits.max_samples,
        0,
        admitted_samples,
    )?;
    work.ensure(
        ImageLimitKind::Conversions,
        limits.max_conversions,
        0,
        admitted_samples,
    )?;
    let worst_case_fuel = output_pixels
        .checked_mul(2)
        .and_then(|value| value.checked_add(admitted_samples.checked_mul(2)?))
        .ok_or(ImageFailure::NumericOverflow)?;
    work.ensure(ImageLimitKind::Fuel, limits.max_fuel, 0, worst_case_fuel)?;

    // Preserve the staged kernel's conservative surface-visit charge without retaining or
    // copying another full-page vector.
    work.charge_fuel(output_pixels)?;
    let Some(device_to_image) = device_to_image else {
        work.check_cancellation()?;
        return Ok(());
    };
    let alpha = NormalizedQ16::from(alpha);
    let blend_mode = ReferenceBlendMode::from(blend_mode);
    for y in 0..output_height {
        for x in 0..output_width {
            let index =
                pixel_index(output_width, output_height, x, y).ok_or(ImageFailure::InvalidImage)?;
            let backdrop_pixel = pixels[index];
            let clip_mask = clip
                .and_then(|mask| mask.sample_mask(x, y))
                .unwrap_or(u64::MAX);
            let mut sums = [0_u64; 4];
            for sample_y in 0..SAMPLE_GRID_WIDTH {
                for sample_x in 0..SAMPLE_GRID_WIDTH {
                    let bit = sample_y
                        .checked_mul(SAMPLE_GRID_WIDTH)
                        .and_then(|value| value.checked_add(sample_x))
                        .ok_or(ImageFailure::NumericOverflow)?;
                    let selected = clip_mask & (1_u64 << bit) != 0;
                    let sample = sample_point(x, y, sample_x, sample_y)?;
                    let image_point = device_to_image.apply(sample)?;
                    let source_index = if selected {
                        image_sample_index(image, image_point.x, image_point.y)?
                    } else {
                        None
                    };
                    let completed = work.guard_sample(source_index.is_some())?;
                    let composed = match source_index {
                        Some(source_index) => {
                            let source =
                                source_pixel(image, source_index)?.with_constant_alpha(alpha);
                            blend_mode.source_over(source, backdrop_pixel)
                        }
                        None => backdrop_pixel,
                    };
                    sums[0] = sums[0]
                        .checked_add(u64::from(composed.red().bits()))
                        .ok_or(ImageFailure::NumericOverflow)?;
                    sums[1] = sums[1]
                        .checked_add(u64::from(composed.green().bits()))
                        .ok_or(ImageFailure::NumericOverflow)?;
                    sums[2] = sums[2]
                        .checked_add(u64::from(composed.blue().bits()))
                        .ok_or(ImageFailure::NumericOverflow)?;
                    sums[3] = sums[3]
                        .checked_add(u64::from(composed.alpha().bits()))
                        .ok_or(ImageFailure::NumericOverflow)?;
                    work.commit_sample(completed.0, completed.1);
                }
            }
            let averaged = averaged_pixel(sums)?;
            work.charge_fuel(1)?;
            pixels[index] = averaged;
        }
    }
    work.check_cancellation()?;
    Ok(())
}

fn image_sample_index(
    image: &ImageResource,
    u: Fixed,
    v: Fixed,
) -> Result<Option<usize>, ImageFailure> {
    let Some(column) = unit_index(u, image.width())? else {
        return Ok(None);
    };
    let Some(bottom_row) = unit_index(v, image.height())? else {
        return Ok(None);
    };
    let row = image
        .height()
        .checked_sub(1)
        .and_then(|last| last.checked_sub(bottom_row))
        .ok_or(ImageFailure::NumericOverflow)?;
    let components = u64::from(image.color_space().components());
    let index = u64::from(row)
        .checked_mul(u64::from(image.width()))
        .and_then(|value| value.checked_add(u64::from(column)))
        .and_then(|value| value.checked_mul(components))
        .ok_or(ImageFailure::NumericOverflow)?;
    usize::try_from(index)
        .map(Some)
        .map_err(|_| ImageFailure::NumericOverflow)
}

pub(crate) fn unit_index(value: Fixed, extent: u32) -> Result<Option<u32>, ImageFailure> {
    if value < Fixed::ZERO || value >= Fixed::ONE {
        return Ok(None);
    }
    let scaled = i128::from(value.raw())
        .checked_mul(i128::from(extent))
        .ok_or(ImageFailure::NumericOverflow)?
        / i128::from(Fixed::ONE.raw());
    u32::try_from(scaled)
        .map(Some)
        .map_err(|_| ImageFailure::NumericOverflow)
}

fn source_pixel(image: &ImageResource, index: usize) -> Result<ReferenceSrgbQ16, ImageFailure> {
    let bytes = image.decoded();
    let channel = |offset: usize| {
        bytes
            .get(index.checked_add(offset)?)
            .copied()
            .map(|value| SceneUnit::from_u16(u16::from(value) * 257))
    };
    let color = match image.color_space() {
        ImageColorSpace::DeviceGray => {
            DeviceColor::Gray(channel(0).ok_or(ImageFailure::InvalidImage)?)
        }
        ImageColorSpace::DeviceRgb => DeviceColor::Rgb {
            red: channel(0).ok_or(ImageFailure::InvalidImage)?,
            green: channel(1).ok_or(ImageFailure::InvalidImage)?,
            blue: channel(2).ok_or(ImageFailure::InvalidImage)?,
        },
        ImageColorSpace::DeviceCmyk => DeviceColor::Cmyk {
            cyan: channel(0).ok_or(ImageFailure::InvalidImage)?,
            magenta: channel(1).ok_or(ImageFailure::InvalidImage)?,
            yellow: channel(2).ok_or(ImageFailure::InvalidImage)?,
            black: channel(3).ok_or(ImageFailure::InvalidImage)?,
        },
    };
    Ok(ReferenceColorProfile::ReferenceColorV1.convert(color))
}

fn averaged_pixel(sums: [u64; 4]) -> Result<PremultipliedRgbaQ16, ImageFailure> {
    let average = |value: u64| {
        let rounded = value
            .checked_add(u64::from(SAMPLES_PER_PIXEL / 2))
            .ok_or(ImageFailure::NumericOverflow)?
            / u64::from(SAMPLES_PER_PIXEL);
        let bits = u32::try_from(rounded).map_err(|_| ImageFailure::NumericOverflow)?;
        NormalizedQ16::from_bits(bits).ok_or(ImageFailure::NumericOverflow)
    };
    PremultipliedRgbaQ16::new(
        average(sums[0])?,
        average(sums[1])?,
        average(sums[2])?,
        average(sums[3])?,
    )
    .ok_or(ImageFailure::NumericOverflow)
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
