use crate::{Color, SkiaError, SkiaErrorCode};

/// Immutable tightly packed straight-RGBA8 bitmap.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Image {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl Image {
    /// Takes ownership of one exact, non-empty RGBA8 pixel buffer.
    pub fn from_rgba8(width: u32, height: u32, pixels: Vec<u8>) -> Result<Self, SkiaError> {
        if width == 0 || height == 0 {
            return Err(SkiaError::new(SkiaErrorCode::InvalidGeometry));
        }
        let expected = u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|value| value.checked_mul(4))
            .ok_or(SkiaError::new(SkiaErrorCode::NumericOverflow))?;
        if usize::try_from(expected).ok() != Some(pixels.len()) {
            return Err(SkiaError::new(SkiaErrorCode::InvalidImage));
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    /// Returns the bitmap width in pixels.
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Returns the bitmap height in pixels.
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Borrows the exact row-major RGBA8 pixels.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    pub(crate) fn color_at(&self, x: u32, y: u32) -> Result<Color, SkiaError> {
        let offset = u64::from(y)
            .checked_mul(u64::from(self.width))
            .and_then(|value| value.checked_add(u64::from(x)))
            .and_then(|value| value.checked_mul(4))
            .ok_or(SkiaError::new(SkiaErrorCode::NumericOverflow))?;
        let offset =
            usize::try_from(offset).map_err(|_| SkiaError::new(SkiaErrorCode::NumericOverflow))?;
        Ok(Color::rgba(
            self.pixels[offset],
            self.pixels[offset + 1],
            self.pixels[offset + 2],
            self.pixels[offset + 3],
        ))
    }
}
