use std::mem::size_of;

use super::{NormalizedQ16, PremultipliedRgbaQ16, ReferenceSrgbQ16};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SurfaceFailure {
    NumericOverflow,
    InvalidSurface,
    Allocation { attempted_bytes: u64 },
}

/// One job-private premultiplied Q16 surface.
///
/// The surface is never exposed outside `ReferenceRenderJob`. Mutating it in place is therefore
/// transactional at the public boundary: any later error drops the complete private value.
pub(super) struct ReferenceSurface {
    width: u32,
    height: u32,
    pixels: Vec<PremultipliedRgbaQ16>,
    retained_bytes: u64,
}

impl ReferenceSurface {
    pub(super) fn new_white(width: u32, height: u32) -> Result<Self, SurfaceFailure> {
        let pixel_count = u64::from(width)
            .checked_mul(u64::from(height))
            .ok_or(SurfaceFailure::NumericOverflow)?;
        if pixel_count == 0 {
            return Err(SurfaceFailure::InvalidSurface);
        }
        let attempted_bytes = pixel_count
            .checked_mul(
                u64::try_from(size_of::<PremultipliedRgbaQ16>())
                    .map_err(|_| SurfaceFailure::NumericOverflow)?,
            )
            .ok_or(SurfaceFailure::NumericOverflow)?;
        let capacity = usize::try_from(pixel_count).map_err(|_| SurfaceFailure::NumericOverflow)?;
        let mut pixels = Vec::new();
        pixels
            .try_reserve_exact(capacity)
            .map_err(|_| SurfaceFailure::Allocation { attempted_bytes })?;
        let retained_bytes = u64::try_from(pixels.capacity())
            .map_err(|_| SurfaceFailure::NumericOverflow)?
            .checked_mul(
                u64::try_from(size_of::<PremultipliedRgbaQ16>())
                    .map_err(|_| SurfaceFailure::NumericOverflow)?,
            )
            .ok_or(SurfaceFailure::NumericOverflow)?;
        let white =
            ReferenceSrgbQ16::gray(NormalizedQ16::ONE).with_constant_alpha(NormalizedQ16::ONE);
        pixels.resize(capacity, white);
        Ok(Self {
            width,
            height,
            pixels,
            retained_bytes,
        })
    }

    pub(super) const fn width(&self) -> u32 {
        self.width
    }

    pub(super) const fn height(&self) -> u32 {
        self.height
    }

    pub(super) fn pixels(&self) -> &[PremultipliedRgbaQ16] {
        &self.pixels
    }

    pub(super) fn pixels_mut(&mut self) -> &mut [PremultipliedRgbaQ16] {
        &mut self.pixels
    }

    pub(super) const fn retained_bytes(&self) -> u64 {
        self.retained_bytes
    }
}
