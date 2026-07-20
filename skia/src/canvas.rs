use std::fmt;

use crate::{BlendMode, Color, FillRule, Paint, Path, PathVerb, Point, Rect, Scalar, Transform};

/// Stable machine-readable canvas failure code.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SkiaErrorCode {
    /// A coordinate or intermediate calculation overflowed.
    NumericOverflow,
    /// A geometry value is invalid.
    InvalidGeometry,
    /// A path operation violates contour ordering.
    InvalidPath,
    /// A configured resource ceiling is invalid.
    InvalidLimits,
    /// A resource ceiling was reached.
    ResourceLimit,
    /// A fallible allocation failed.
    AllocationFailed,
    /// A stack restore was requested without a matching save.
    RestoreUnderflow,
    /// The requested operation needs a not-yet-implemented transform mode.
    UnsupportedTransform,
}

/// Source-redacted graphics error.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SkiaError {
    code: SkiaErrorCode,
}

impl SkiaError {
    pub(crate) const fn new(code: SkiaErrorCode) -> Self {
        Self { code }
    }

    /// Returns the stable failure code.
    pub const fn code(self) -> SkiaErrorCode {
        self.code
    }
}

impl fmt::Display for SkiaError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}", self.code)
    }
}

impl std::error::Error for SkiaError {}

/// Limits for one CPU-owned RGBA8 surface and Canvas state stack.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SurfaceLimits {
    max_pixels: u64,
    max_bytes: u64,
    max_save_depth: usize,
}

impl SurfaceLimits {
    /// Creates checked limits.
    pub fn new(max_pixels: u64, max_bytes: u64, max_save_depth: usize) -> Result<Self, SkiaError> {
        if max_pixels == 0 || max_bytes == 0 || max_save_depth == 0 {
            return Err(SkiaError::new(SkiaErrorCode::InvalidLimits));
        }
        Ok(Self {
            max_pixels,
            max_bytes,
            max_save_depth,
        })
    }
}

impl Default for SurfaceLimits {
    fn default() -> Self {
        Self {
            max_pixels: 67_108_864,
            max_bytes: 256 * 1024 * 1024,
            max_save_depth: 256,
        }
    }
}

/// Complete mutable CPU surface with top-left, tightly packed straight RGBA8 pixels.
#[derive(Debug)]
pub struct Surface {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
    limits: SurfaceLimits,
}

impl Surface {
    /// Allocates a transparent, bounded CPU surface.
    pub fn new(width: u32, height: u32, limits: SurfaceLimits) -> Result<Self, SkiaError> {
        if width == 0 || height == 0 {
            return Err(SkiaError::new(SkiaErrorCode::InvalidGeometry));
        }
        let pixels = u64::from(width)
            .checked_mul(u64::from(height))
            .ok_or(SkiaError::new(SkiaErrorCode::NumericOverflow))?;
        let bytes = pixels
            .checked_mul(4)
            .ok_or(SkiaError::new(SkiaErrorCode::NumericOverflow))?;
        if pixels > limits.max_pixels || bytes > limits.max_bytes {
            return Err(SkiaError::new(SkiaErrorCode::ResourceLimit));
        }
        let length =
            usize::try_from(bytes).map_err(|_| SkiaError::new(SkiaErrorCode::ResourceLimit))?;
        let mut output = Vec::new();
        output
            .try_reserve_exact(length)
            .map_err(|_| SkiaError::new(SkiaErrorCode::AllocationFailed))?;
        output.resize(length, 0);
        Ok(Self {
            width,
            height,
            pixels: output,
            limits,
        })
    }

    /// Returns the device width in pixels.
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Returns the device height in pixels.
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Borrows the exact row-major RGBA8 pixels.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Starts one canvas state scope over this surface.
    pub fn canvas(&mut self) -> Canvas<'_> {
        let clip = DeviceRect {
            left: 0,
            top: 0,
            right: i64::from(self.width),
            bottom: i64::from(self.height),
        };
        Canvas {
            surface: self,
            state: State {
                transform: Transform::IDENTITY,
                clip,
            },
            saves: Vec::new(),
        }
    }
}

/// Axis-aligned clipping rectangle in canvas coordinates.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ClipRect(Rect);

impl ClipRect {
    /// Creates a positive-area clip rectangle.
    pub const fn new(rect: Rect) -> Self {
        Self(rect)
    }
}

#[derive(Clone, Copy, Debug)]
struct State {
    transform: Transform,
    clip: DeviceRect,
}

/// Mutable CPU drawing context.
pub struct Canvas<'a> {
    surface: &'a mut Surface,
    state: State,
    saves: Vec<State>,
}

impl Canvas<'_> {
    /// Clears all pixels, ignoring the current transform and clip.
    pub fn clear(&mut self, color: Color) {
        for pixel in self.surface.pixels.chunks_exact_mut(4) {
            pixel.copy_from_slice(&color.channels());
        }
    }

    /// Saves the current transform and clip state.
    pub fn save(&mut self) -> Result<(), SkiaError> {
        if self.saves.len() == self.surface.limits.max_save_depth {
            return Err(SkiaError::new(SkiaErrorCode::ResourceLimit));
        }
        self.saves
            .try_reserve(1)
            .map_err(|_| SkiaError::new(SkiaErrorCode::AllocationFailed))?;
        self.saves.push(self.state);
        Ok(())
    }

    /// Restores the most recently saved state.
    pub fn restore(&mut self) -> Result<(), SkiaError> {
        self.state = self
            .saves
            .pop()
            .ok_or(SkiaError::new(SkiaErrorCode::RestoreUnderflow))?;
        Ok(())
    }

    /// Replaces the current affine transform.
    pub fn set_transform(&mut self, transform: Transform) {
        self.state.transform = transform;
    }

    /// Intersects the current clip with one transformed axis-aligned rectangle.
    pub fn clip_rect(&mut self, clip: ClipRect) -> Result<(), SkiaError> {
        if !self.state.transform.is_axis_aligned() {
            return Err(SkiaError::new(SkiaErrorCode::UnsupportedTransform));
        }
        self.state.clip = self.state.clip.intersection(self.device_rect(clip.0)?);
        Ok(())
    }

    /// Fills one transformed axis-aligned rectangle.
    pub fn fill_rect(&mut self, rect: Rect, paint: Paint) -> Result<(), SkiaError> {
        if !self.state.transform.is_axis_aligned() {
            return Err(SkiaError::new(SkiaErrorCode::UnsupportedTransform));
        }
        self.fill_device_rect(self.device_rect(rect)?.intersection(self.state.clip), paint)
    }

    /// Fills a transformed line path using the selected winding rule.
    pub fn fill_path(
        &mut self,
        path: &Path,
        rule: FillRule,
        paint: Paint,
    ) -> Result<(), SkiaError> {
        let contours = transformed_contours(path, self.state.transform)?;
        let bounds = contour_bounds(&contours).intersection(self.state.clip);
        for y in bounds.top..bounds.bottom {
            for x in bounds.left..bounds.right {
                let sample = Point::new(
                    Scalar::from_ratio(
                        x.checked_mul(2)
                            .ok_or(SkiaError::new(SkiaErrorCode::NumericOverflow))?
                            .checked_add(1)
                            .ok_or(SkiaError::new(SkiaErrorCode::NumericOverflow))?,
                        2,
                    )?,
                    Scalar::from_ratio(
                        y.checked_mul(2)
                            .ok_or(SkiaError::new(SkiaErrorCode::NumericOverflow))?
                            .checked_add(1)
                            .ok_or(SkiaError::new(SkiaErrorCode::NumericOverflow))?,
                        2,
                    )?,
                );
                if contains(&contours, sample, rule)? {
                    self.blend_pixel(x, y, paint)?;
                }
            }
        }
        Ok(())
    }

    fn device_rect(&self, rect: Rect) -> Result<DeviceRect, SkiaError> {
        let first = self
            .state
            .transform
            .map_point(Point::new(rect.left(), rect.top()))?;
        let second = self
            .state
            .transform
            .map_point(Point::new(rect.right(), rect.bottom()))?;
        Ok(DeviceRect {
            left: floor_q16(first.x().bits()),
            top: floor_q16(first.y().bits()),
            right: ceil_q16(second.x().bits()),
            bottom: ceil_q16(second.y().bits()),
        }
        .normalized())
    }

    fn fill_device_rect(&mut self, rectangle: DeviceRect, paint: Paint) -> Result<(), SkiaError> {
        for y in rectangle.top..rectangle.bottom {
            for x in rectangle.left..rectangle.right {
                self.blend_pixel(x, y, paint)?;
            }
        }
        Ok(())
    }

    fn blend_pixel(&mut self, x: i64, y: i64, paint: Paint) -> Result<(), SkiaError> {
        if x < 0
            || y < 0
            || x >= i64::from(self.surface.width)
            || y >= i64::from(self.surface.height)
        {
            return Ok(());
        }
        let index = y
            .checked_mul(i64::from(self.surface.width))
            .and_then(|value| value.checked_add(x))
            .and_then(|value| value.checked_mul(4))
            .ok_or(SkiaError::new(SkiaErrorCode::NumericOverflow))?;
        let index =
            usize::try_from(index).map_err(|_| SkiaError::new(SkiaErrorCode::NumericOverflow))?;
        let destination = Color::rgba(
            self.surface.pixels[index],
            self.surface.pixels[index + 1],
            self.surface.pixels[index + 2],
            self.surface.pixels[index + 3],
        );
        let result = match paint.blend_mode() {
            BlendMode::SourceOver => source_over(paint.color(), destination),
        };
        self.surface.pixels[index..index + 4].copy_from_slice(&result.channels());
        Ok(())
    }
}

#[derive(Clone, Copy, Debug)]
struct DeviceRect {
    left: i64,
    top: i64,
    right: i64,
    bottom: i64,
}

impl DeviceRect {
    fn normalized(self) -> Self {
        Self {
            left: self.left.min(self.right),
            top: self.top.min(self.bottom),
            right: self.left.max(self.right),
            bottom: self.top.max(self.bottom),
        }
    }

    fn intersection(self, other: Self) -> Self {
        let left = self.left.max(other.left);
        let top = self.top.max(other.top);
        let right = self.right.min(other.right).max(left);
        let bottom = self.bottom.min(other.bottom).max(top);
        Self {
            left,
            top,
            right,
            bottom,
        }
    }
}

fn transformed_contours(path: &Path, transform: Transform) -> Result<Vec<Vec<Point>>, SkiaError> {
    let mut contours = Vec::new();
    let mut current = Vec::new();
    for verb in path.verbs() {
        match *verb {
            PathVerb::MoveTo(point) => {
                if !current.is_empty() {
                    contours.push(current);
                    current = Vec::new();
                }
                current.push(transform.map_point(point)?);
            }
            PathVerb::LineTo(point) => current.push(transform.map_point(point)?),
            PathVerb::Close => {
                if !current.is_empty() {
                    contours.push(current);
                    current = Vec::new();
                }
            }
        }
    }
    if !current.is_empty() {
        contours.push(current);
    }
    if contours.iter().all(|contour| contour.len() < 3) {
        return Err(SkiaError::new(SkiaErrorCode::InvalidPath));
    }
    Ok(contours)
}

fn contour_bounds(contours: &[Vec<Point>]) -> DeviceRect {
    let mut left = i64::MAX;
    let mut top = i64::MAX;
    let mut right = i64::MIN;
    let mut bottom = i64::MIN;
    for point in contours.iter().flatten() {
        left = left.min(i64::from(floor_q16(point.x().bits())));
        top = top.min(i64::from(floor_q16(point.y().bits())));
        right = right.max(i64::from(ceil_q16(point.x().bits())));
        bottom = bottom.max(i64::from(ceil_q16(point.y().bits())));
    }
    DeviceRect {
        left,
        top,
        right,
        bottom,
    }
}

fn contains(contours: &[Vec<Point>], sample: Point, rule: FillRule) -> Result<bool, SkiaError> {
    let mut parity = false;
    let mut winding = 0_i32;
    for contour in contours {
        if contour.len() < 3 {
            continue;
        }
        for (start, end) in contour
            .iter()
            .copied()
            .zip(contour.iter().copied().cycle().skip(1))
            .take(contour.len())
        {
            let start_y = i64::from(start.y().bits());
            let end_y = i64::from(end.y().bits());
            let sample_y = i64::from(sample.y().bits());
            let rising = start_y <= sample_y && sample_y < end_y;
            let falling = end_y <= sample_y && sample_y < start_y;
            if !(rising || falling) {
                continue;
            }
            let dy = end_y
                .checked_sub(start_y)
                .ok_or(SkiaError::new(SkiaErrorCode::NumericOverflow))?;
            let numerator = i128::from(start.x().bits())
                .checked_mul(i128::from(dy))
                .and_then(|value| {
                    i128::from(sample_y - start_y)
                        .checked_mul(i128::from(
                            i64::from(end.x().bits()) - i64::from(start.x().bits()),
                        ))
                        .and_then(|delta| value.checked_add(delta))
                })
                .ok_or(SkiaError::new(SkiaErrorCode::NumericOverflow))?;
            let right_of_sample = if dy > 0 {
                numerator > i128::from(sample.x().bits()) * i128::from(dy)
            } else {
                numerator < i128::from(sample.x().bits()) * i128::from(dy)
            };
            if right_of_sample {
                parity = !parity;
                winding += if rising { 1 } else { -1 };
            }
        }
    }
    Ok(match rule {
        FillRule::EvenOdd => parity,
        FillRule::NonZero => winding != 0,
    })
}

fn source_over(source: Color, destination: Color) -> Color {
    let [sr, sg, sb, sa] = source.channels().map(u32::from);
    let [dr, dg, db, da] = destination.channels().map(u32::from);
    let inverse_source_alpha = u32::from(u8::MAX) - sa;
    let alpha = sa + rounded_div(da * inverse_source_alpha, u32::from(u8::MAX));
    if alpha == 0 {
        return Color::TRANSPARENT;
    }
    let channel = |source: u32, destination: u32| {
        let source = rounded_div(source * sa, u32::from(u8::MAX));
        let destination = rounded_div(destination * da, u32::from(u8::MAX));
        let premultiplied =
            source + rounded_div(destination * inverse_source_alpha, u32::from(u8::MAX));
        u8::try_from(rounded_div(premultiplied * u32::from(u8::MAX), alpha)).unwrap_or(u8::MAX)
    };
    Color::rgba(
        channel(sr, dr),
        channel(sg, dg),
        channel(sb, db),
        u8::try_from(alpha).unwrap_or(u8::MAX),
    )
}

fn rounded_div(numerator: u32, denominator: u32) -> u32 {
    (numerator + denominator / 2) / denominator
}

fn floor_q16(value: i32) -> i64 {
    let value = i64::from(value);
    if value >= 0 {
        value >> 16
    } else {
        -((-value + 65_535) >> 16)
    }
}

fn ceil_q16(value: i32) -> i64 {
    -floor_q16(value.saturating_neg())
}
