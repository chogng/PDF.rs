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
    pixel_count: usize,
    pixels: Vec<PremultipliedRgbaQ16>,
    retained_bytes: u64,
}

impl ReferenceSurface {
    /// Reserves the complete private surface without initializing any pixel values.
    ///
    /// The renderer grows the initialized prefix in bounded chunks so that initialization fuel
    /// and cancellation remain owned by `RenderWork`.
    pub(super) fn reserve(width: u32, height: u32) -> Result<Self, SurfaceFailure> {
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
        Ok(Self {
            width,
            height,
            pixel_count: capacity,
            pixels,
            retained_bytes,
        })
    }

    /// Appends exactly `additional` white pixels without growing the reserved allocation.
    pub(super) fn initialize_white(&mut self, additional: usize) -> Result<(), SurfaceFailure> {
        let target = self
            .pixels
            .len()
            .checked_add(additional)
            .ok_or(SurfaceFailure::NumericOverflow)?;
        if target > self.pixel_count {
            return Err(SurfaceFailure::InvalidSurface);
        }
        let white =
            ReferenceSrgbQ16::gray(NormalizedQ16::ONE).with_constant_alpha(NormalizedQ16::ONE);
        self.pixels.resize(target, white);
        Ok(())
    }

    /// Returns whether every reserved pixel has been initialized.
    pub(super) fn is_initialized(&self) -> bool {
        self.pixels.len() == self.pixel_count
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

#[cfg(test)]
mod tests {
    use std::mem::size_of;

    use super::{PremultipliedRgbaQ16, ReferenceSurface, SurfaceFailure};

    #[test]
    fn semantic_initialization_is_independent_of_allocator_overcapacity() {
        let pixels = Vec::<PremultipliedRgbaQ16>::with_capacity(4);
        assert!(pixels.capacity() >= 4);
        let retained_bytes = u64::try_from(pixels.capacity()).unwrap()
            * u64::try_from(size_of::<PremultipliedRgbaQ16>()).unwrap();
        let mut surface = ReferenceSurface {
            width: 2,
            height: 1,
            pixel_count: 2,
            pixels,
            retained_bytes,
        };

        assert!(!surface.is_initialized());
        assert_eq!(surface.initialize_white(2), Ok(()));
        assert!(surface.is_initialized());
        assert_eq!(surface.pixels().len(), 2);
        assert_eq!(
            surface.initialize_white(1),
            Err(SurfaceFailure::InvalidSurface)
        );
    }
}
