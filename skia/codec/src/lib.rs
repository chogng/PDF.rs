//! Decoding of general-purpose encoded image bytes into Skia image resources.
//!
//! This crate deliberately owns codec policy rather than pixel storage. Its
//! output is always the portable, tightly packed, straight-alpha RGBA8
//! [`Image`]. Rendering backends therefore never parse untrusted image files.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use std::{fmt, io::Cursor};

use image::{ImageError, ImageFormat, ImageReader, Limits};
use pdf_rs_skia_image::Image;

/// Stable machine-readable image codec failure code.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CodecErrorCode {
    /// Codec limits are empty or otherwise invalid.
    InvalidLimits,
    /// The encoded input exceeds its configured byte ceiling.
    InputTooLarge,
    /// The decoded dimensions or RGBA8 payload exceed configured limits.
    ImageTooLarge,
    /// The encoded bytes do not identify a supported image format.
    UnsupportedFormat,
    /// A supported encoded image could not be decoded.
    DecodeFailed,
}

/// Source-redacted image codec error.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CodecError {
    code: CodecErrorCode,
}

impl CodecError {
    /// Creates an error with a stable code.
    pub const fn new(code: CodecErrorCode) -> Self {
        Self { code }
    }

    /// Returns the stable error code.
    pub const fn code(self) -> CodecErrorCode {
        self.code
    }
}

impl fmt::Display for CodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}", self.code)
    }
}

impl std::error::Error for CodecError {}

/// Resource ceilings applied while decoding untrusted encoded image bytes.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct CodecLimits {
    max_input_bytes: usize,
    max_pixels: u64,
    max_decoded_bytes: u64,
}

impl CodecLimits {
    /// Creates non-zero codec resource limits.
    pub fn new(
        max_input_bytes: usize,
        max_pixels: u64,
        max_decoded_bytes: u64,
    ) -> Result<Self, CodecError> {
        if max_input_bytes == 0 || max_pixels == 0 || max_decoded_bytes == 0 {
            return Err(CodecError::new(CodecErrorCode::InvalidLimits));
        }
        Ok(Self {
            max_input_bytes,
            max_pixels,
            max_decoded_bytes,
        })
    }

    /// Returns the maximum accepted encoded input length in bytes.
    pub const fn max_input_bytes(self) -> usize {
        self.max_input_bytes
    }

    /// Returns the maximum accepted decoded pixel count.
    pub const fn max_pixels(self) -> u64 {
        self.max_pixels
    }

    /// Returns the maximum accepted decoded RGBA8 byte count.
    pub const fn max_decoded_bytes(self) -> u64 {
        self.max_decoded_bytes
    }
}

impl Default for CodecLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 64 * 1024 * 1024,
            max_pixels: 67_108_864,
            max_decoded_bytes: 256 * 1024 * 1024,
        }
    }
}

/// Stateless codec facade for decoding common image formats.
pub struct ImageCodec;

impl ImageCodec {
    /// Decodes PNG, JPEG, or WebP bytes using [`CodecLimits::default`].
    pub fn decode(bytes: &[u8]) -> Result<Image, CodecError> {
        Self::decode_with_limits(bytes, CodecLimits::default())
    }

    /// Detects and decodes PNG, JPEG, or WebP bytes subject to `limits`.
    ///
    /// The result is always straight-alpha RGBA8, including for opaque input.
    pub fn decode_with_limits(bytes: &[u8], limits: CodecLimits) -> Result<Image, CodecError> {
        if bytes.len() > limits.max_input_bytes {
            return Err(CodecError::new(CodecErrorCode::InputTooLarge));
        }

        let mut reader = ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .map_err(|_| CodecError::new(CodecErrorCode::DecodeFailed))?;
        if !matches!(
            reader.format(),
            Some(ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP)
        ) {
            return Err(CodecError::new(CodecErrorCode::UnsupportedFormat));
        }

        let maximum_dimension = u32::try_from(limits.max_pixels).unwrap_or(u32::MAX);
        let mut decoder_limits = Limits::default();
        decoder_limits.max_image_width = Some(maximum_dimension);
        decoder_limits.max_image_height = Some(maximum_dimension);
        decoder_limits.max_alloc = Some(limits.max_decoded_bytes);
        reader.limits(decoder_limits);
        let decoded = reader.decode().map_err(|error| match error {
            ImageError::Limits(_) => CodecError::new(CodecErrorCode::ImageTooLarge),
            _ => CodecError::new(CodecErrorCode::DecodeFailed),
        })?;
        let width = decoded.width();
        let height = decoded.height();
        let pixels = u64::from(width)
            .checked_mul(u64::from(height))
            .ok_or(CodecError::new(CodecErrorCode::ImageTooLarge))?;
        let byte_count = pixels
            .checked_mul(4)
            .ok_or(CodecError::new(CodecErrorCode::ImageTooLarge))?;
        if pixels > limits.max_pixels || byte_count > limits.max_decoded_bytes {
            return Err(CodecError::new(CodecErrorCode::ImageTooLarge));
        }

        let rgba8 = decoded.into_rgba8().into_raw();
        let image = Image::from_rgba8(width, height, rgba8)
            .map_err(|_| CodecError::new(CodecErrorCode::DecodeFailed))?;
        Ok(image)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use image::{DynamicImage, ImageFormat, RgbaImage};

    use super::{CodecErrorCode, CodecLimits, ImageCodec};

    fn encoded(format: ImageFormat) -> Vec<u8> {
        let source = RgbaImage::from_raw(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 128]).unwrap();
        let mut bytes = Vec::new();
        DynamicImage::ImageRgba8(source)
            .write_to(&mut Cursor::new(&mut bytes), format)
            .unwrap();
        bytes
    }

    #[test]
    fn decodes_png_jpeg_and_webp_to_rgba8_resources() {
        for format in [ImageFormat::Png, ImageFormat::Jpeg, ImageFormat::WebP] {
            let image = ImageCodec::decode(&encoded(format)).unwrap();
            assert_eq!((image.width(), image.height()), (2, 1));
            assert_eq!(image.pixel_at(0, 0).unwrap()[3], 255);
        }
    }

    #[test]
    fn decoder_rejects_unknown_and_over_budget_input() {
        assert_eq!(
            ImageCodec::decode(b"not an image").unwrap_err().code(),
            CodecErrorCode::UnsupportedFormat
        );
        let limits = CodecLimits::new(4, 4, 16).unwrap();
        assert_eq!(
            ImageCodec::decode_with_limits(&encoded(ImageFormat::Png), limits)
                .unwrap_err()
                .code(),
            CodecErrorCode::InputTooLarge
        );
    }

    #[test]
    fn decoder_enforces_decoded_pixel_budget() {
        let limits = CodecLimits::new(1024, 1, 4).unwrap();
        assert_eq!(
            ImageCodec::decode_with_limits(&encoded(ImageFormat::Png), limits)
                .unwrap_err()
                .code(),
            CodecErrorCode::ImageTooLarge
        );
    }
}
