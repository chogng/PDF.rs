//! Immutable, owned RGBA8 image resources.
//!
//! This crate has no dependency on the drawing core or a rendering backend, so
//! image decoding and storage remain usable independently of display lists.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::fmt;

/// Stable machine-readable image creation failure code.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ImageErrorCode {
    /// Image dimensions are empty.
    InvalidDimensions,
    /// Dimensions cannot be represented by the host or their byte count overflowed.
    NumericOverflow,
    /// Pixel storage does not exactly match tightly packed RGBA8 dimensions.
    InvalidPixels,
}

/// Source-redacted image creation error.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ImageError {
    code: ImageErrorCode,
}

impl ImageError {
    /// Creates an error with a stable code.
    pub const fn new(code: ImageErrorCode) -> Self {
        Self { code }
    }

    /// Returns the stable error code.
    pub const fn code(self) -> ImageErrorCode {
        self.code
    }
}

impl fmt::Display for ImageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}", self.code)
    }
}

impl std::error::Error for ImageError {}

/// Immutable, tightly packed, straight-alpha RGBA8 image data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Image {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl Image {
    /// Takes ownership of one exact, non-empty RGBA8 pixel buffer.
    pub fn from_rgba8(width: u32, height: u32, pixels: Vec<u8>) -> Result<Self, ImageError> {
        if width == 0 || height == 0 {
            return Err(ImageError::new(ImageErrorCode::InvalidDimensions));
        }
        let expected = u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|value| value.checked_mul(4))
            .ok_or(ImageError::new(ImageErrorCode::NumericOverflow))?;
        if usize::try_from(expected).ok() != Some(pixels.len()) {
            return Err(ImageError::new(ImageErrorCode::InvalidPixels));
        }
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    /// Returns the image width in pixels.
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Returns the image height in pixels.
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Borrows the exact row-major RGBA8 pixels.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Returns one RGBA8 pixel, or `None` when coordinates are out of bounds.
    pub fn pixel_at(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let offset = (usize::try_from(y).ok()? * usize::try_from(self.width).ok()?
            + usize::try_from(x).ok()?)
            * 4;
        self.pixels.get(offset..offset + 4)?.try_into().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::{Image, ImageErrorCode};

    #[test]
    fn image_owns_exact_rgba8_pixels() {
        let image = Image::from_rgba8(2, 1, vec![1, 2, 3, 4, 5, 6, 7, 8]).unwrap();

        assert_eq!(image.pixels(), &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(image.pixel_at(1, 0), Some([5, 6, 7, 8]));
        assert_eq!(image.pixel_at(2, 0), None);
        assert_eq!(image.pixel_at(0, 1), None);
    }

    #[test]
    fn image_rejects_empty_and_mismatched_storage() {
        assert_eq!(
            Image::from_rgba8(0, 1, Vec::new()).unwrap_err().code(),
            ImageErrorCode::InvalidDimensions
        );
        assert_eq!(
            Image::from_rgba8(2, 2, vec![0; 3]).unwrap_err().code(),
            ImageErrorCode::InvalidPixels
        );
    }
}
