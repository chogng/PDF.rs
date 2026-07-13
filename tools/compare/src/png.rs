use std::error::Error;
use std::fmt;

use crate::pixel::{
    PixelArtifact, PixelBufferRole, PixelDiffSummary, PixelError, compare_pixels, validate_rgba_len,
};

const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
const STORED_BLOCK_MAX: usize = u16::MAX as usize;

/// Failure while producing a deterministic PNG artifact.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PngError {
    /// Invalid dimensions, input length, or comparison dimensions.
    Pixel(PixelError),
    /// A PNG or zlib size calculation overflowed.
    ArithmeticOverflow {
        /// Name of the failed checked calculation.
        operation: &'static str,
    },
    /// PNG chunk lengths are limited to an unsigned 32-bit value.
    ChunkTooLarge {
        /// Attempted IDAT payload byte length.
        length: usize,
    },
    /// The encoder could not reserve a deterministic output buffer.
    AllocationFailed {
        /// Requested deterministic output capacity.
        requested: usize,
    },
}

impl fmt::Display for PngError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pixel(error) => write!(formatter, "{error}"),
            Self::ArithmeticOverflow { operation } => {
                write!(
                    formatter,
                    "PNG arithmetic overflow while computing {operation}"
                )
            }
            Self::ChunkTooLarge { length } => write!(
                formatter,
                "PNG IDAT payload of {length} bytes exceeds the chunk length limit"
            ),
            Self::AllocationFailed { requested } => write!(
                formatter,
                "could not reserve {requested} bytes for deterministic PNG output"
            ),
        }
    }
}

impl Error for PngError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Pixel(error) => Some(error),
            Self::ArithmeticOverflow { .. }
            | Self::ChunkTooLarge { .. }
            | Self::AllocationFailed { .. } => None,
        }
    }
}

impl From<PixelError> for PngError {
    fn from(error: PixelError) -> Self {
        Self::Pixel(error)
    }
}

/// Deterministic PNG bytes for a native image, baseline image, and exact diff.
#[derive(Clone, Eq, PartialEq)]
pub struct PixelPngBundle {
    native_png: Vec<u8>,
    baseline_png: Vec<u8>,
    difference_png: Vec<u8>,
    summary: PixelDiffSummary,
}

impl fmt::Debug for PixelPngBundle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PixelPngBundle")
            .field("native_png_bytes", &self.native_png.len())
            .field("baseline_png_bytes", &self.baseline_png.len())
            .field("difference_png_bytes", &self.difference_png.len())
            .field("summary", &self.summary)
            .field("content", &"[REDACTED]")
            .finish()
    }
}

impl PixelPngBundle {
    /// Returns the encoded native image.
    pub fn native_png(&self) -> &[u8] {
        &self.native_png
    }

    /// Returns the encoded baseline image.
    pub fn baseline_png(&self) -> &[u8] {
        &self.baseline_png
    }

    /// Returns the encoded visible difference image.
    ///
    /// RGB stores absolute channel deltas. Differing pixels are opaque so an
    /// equal source alpha cannot hide a real difference.
    pub fn difference_png(&self) -> &[u8] {
        &self.difference_png
    }

    /// Returns exact RGBA comparison statistics.
    pub const fn summary(&self) -> &PixelDiffSummary {
        &self.summary
    }
}

/// Encodes native, baseline, and a visible exact-difference PNG artifact.
pub fn encode_pixel_comparison_pngs(
    native: &PixelArtifact,
    baseline: &PixelArtifact,
) -> Result<PixelPngBundle, PngError> {
    let difference = compare_pixels(native, baseline)?;
    let native_png = encode_rgba_png(native.width(), native.height(), native.rgba())?;
    let baseline_png = encode_rgba_png(baseline.width(), baseline.height(), baseline.rgba())?;
    let visible_difference = visible_difference_rgba(difference.absolute_difference().rgba())?;
    let difference_png = encode_rgba_png(
        difference.absolute_difference().width(),
        difference.absolute_difference().height(),
        &visible_difference,
    )?;

    Ok(PixelPngBundle {
        native_png,
        baseline_png,
        difference_png,
        summary: difference.summary().clone(),
    })
}

fn visible_difference_rgba(absolute_rgba: &[u8]) -> Result<Vec<u8>, PngError> {
    let mut visible = Vec::new();
    visible
        .try_reserve_exact(absolute_rgba.len())
        .map_err(|_| PngError::AllocationFailed {
            requested: absolute_rgba.len(),
        })?;
    visible.extend_from_slice(absolute_rgba);
    for pixel in visible.chunks_exact_mut(4) {
        let alpha_delta = pixel[3];
        let differs = pixel.iter().any(|channel| *channel != 0);
        if alpha_delta != 0 && pixel[..3].iter().all(|channel| *channel == 0) {
            pixel[..3].fill(alpha_delta);
        }
        pixel[3] = if differs { 255 } else { 0 };
    }
    Ok(visible)
}

/// Encodes row-major RGBA8 pixels as a deterministic, standards-valid PNG.
///
/// The encoder uses filter type 0 for every row, one IDAT chunk, a zlib stream
/// with DEFLATE stored blocks, and no optional PNG chunks.
pub fn encode_rgba_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, PngError> {
    validate_rgba_len(width, height, rgba.len(), PixelBufferRole::PngInput)?;

    let width = usize::try_from(width).map_err(|_| PngError::ArithmeticOverflow {
        operation: "row width",
    })?;
    let height = usize::try_from(height).map_err(|_| PngError::ArithmeticOverflow {
        operation: "row count",
    })?;
    let row_bytes = width.checked_mul(4).ok_or(PngError::ArithmeticOverflow {
        operation: "row byte length",
    })?;
    let filtered_row_bytes = row_bytes
        .checked_add(1)
        .ok_or(PngError::ArithmeticOverflow {
            operation: "filtered row byte length",
        })?;
    let raw_length =
        filtered_row_bytes
            .checked_mul(height)
            .ok_or(PngError::ArithmeticOverflow {
                operation: "filtered image byte length",
            })?;
    let block_count =
        raw_length
            .checked_add(STORED_BLOCK_MAX - 1)
            .ok_or(PngError::ArithmeticOverflow {
                operation: "stored block count",
            })?
            / STORED_BLOCK_MAX;
    let stored_overhead = block_count
        .checked_mul(5)
        .ok_or(PngError::ArithmeticOverflow {
            operation: "stored block overhead",
        })?;
    let zlib_length = raw_length
        .checked_add(stored_overhead)
        .and_then(|length| length.checked_add(6))
        .ok_or(PngError::ArithmeticOverflow {
            operation: "zlib stream length",
        })?;
    if u32::try_from(zlib_length).is_err() {
        return Err(PngError::ChunkTooLarge {
            length: zlib_length,
        });
    }

    let mut raw = reserve_vec(raw_length)?;
    for row in rgba.chunks_exact(row_bytes) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    debug_assert_eq!(raw.len(), raw_length);

    let zlib = encode_stored_zlib(&raw, zlib_length)?;
    let png_length = 8usize
        .checked_add(12 + 13)
        .and_then(|length| length.checked_add(12))
        .and_then(|length| length.checked_add(zlib_length))
        .and_then(|length| length.checked_add(12))
        .ok_or(PngError::ArithmeticOverflow {
            operation: "PNG output length",
        })?;
    let mut png = reserve_vec(png_length)?;
    png.extend_from_slice(PNG_SIGNATURE);

    let mut header = [0u8; 13];
    header[0..4].copy_from_slice(&u32::try_from(width).unwrap_or(u32::MAX).to_be_bytes());
    header[4..8].copy_from_slice(&u32::try_from(height).unwrap_or(u32::MAX).to_be_bytes());
    header[8] = 8;
    header[9] = 6;
    append_chunk(&mut png, *b"IHDR", &header)?;
    append_chunk(&mut png, *b"IDAT", &zlib)?;
    append_chunk(&mut png, *b"IEND", &[])?;
    debug_assert_eq!(png.len(), png_length);

    Ok(png)
}

/// Computes the IEEE CRC-32 used by PNG chunks.
pub fn crc32(bytes: &[u8]) -> u32 {
    crc32_parts(&[bytes])
}

/// Computes the Adler-32 checksum used by zlib streams.
pub fn adler32(bytes: &[u8]) -> u32 {
    const MODULUS: u32 = 65_521;
    let mut a = 1u32;
    let mut b = 0u32;

    for byte in bytes {
        a = (a + u32::from(*byte)) % MODULUS;
        b = (b + a) % MODULUS;
    }

    (b << 16) | a
}

fn reserve_vec(capacity: usize) -> Result<Vec<u8>, PngError> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(capacity)
        .map_err(|_| PngError::AllocationFailed {
            requested: capacity,
        })?;
    Ok(bytes)
}

fn encode_stored_zlib(raw: &[u8], expected_length: usize) -> Result<Vec<u8>, PngError> {
    let mut encoded = reserve_vec(expected_length)?;
    encoded.extend_from_slice(&[0x78, 0x01]);

    let mut offset = 0usize;
    while offset < raw.len() {
        let remaining = raw.len() - offset;
        let block_length = remaining.min(STORED_BLOCK_MAX);
        let final_block = block_length == remaining;
        encoded.push(u8::from(final_block));

        let length = u16::try_from(block_length).map_err(|_| PngError::ArithmeticOverflow {
            operation: "stored block length",
        })?;
        encoded.extend_from_slice(&length.to_le_bytes());
        encoded.extend_from_slice(&(!length).to_le_bytes());
        encoded.extend_from_slice(&raw[offset..offset + block_length]);
        offset += block_length;
    }

    encoded.extend_from_slice(&adler32(raw).to_be_bytes());
    debug_assert_eq!(encoded.len(), expected_length);
    Ok(encoded)
}

fn append_chunk(output: &mut Vec<u8>, kind: [u8; 4], data: &[u8]) -> Result<(), PngError> {
    let length =
        u32::try_from(data.len()).map_err(|_| PngError::ChunkTooLarge { length: data.len() })?;
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(&kind);
    output.extend_from_slice(data);
    output.extend_from_slice(&crc32_parts(&[&kind, data]).to_be_bytes());
    Ok(())
}

fn crc32_parts(parts: &[&[u8]]) -> u32 {
    const POLYNOMIAL: u32 = 0xedb8_8320;
    let mut checksum = u32::MAX;

    for part in parts {
        for byte in *part {
            checksum ^= u32::from(*byte);
            for _ in 0..8 {
                let mask = 0u32.wrapping_sub(checksum & 1);
                checksum = (checksum >> 1) ^ (POLYNOMIAL & mask);
            }
        }
    }

    !checksum
}

#[cfg(test)]
mod tests {
    use super::{
        PNG_SIGNATURE, PngError, adler32, crc32, encode_pixel_comparison_pngs, encode_rgba_png,
    };
    use crate::{PixelArtifact, PixelBufferRole, PixelError};

    #[derive(Debug)]
    struct Chunk<'a> {
        kind: [u8; 4],
        data: &'a [u8],
    }

    #[test]
    fn checksum_algorithms_match_standard_check_values() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        assert_eq!(adler32(b"Wikipedia"), 0x11e6_0398);
    }

    #[test]
    fn png_has_valid_signature_chunks_checksums_and_scanlines() {
        let rgba = [255, 0, 0, 255, 0, 255, 0, 128];
        let png = encode_rgba_png(2, 1, &rgba).expect("valid RGBA encodes");
        assert_eq!(&png[..PNG_SIGNATURE.len()], PNG_SIGNATURE);

        let chunks = parse_chunks(&png);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].kind, *b"IHDR");
        assert_eq!(chunks[0].data, &[0, 0, 0, 2, 0, 0, 0, 1, 8, 6, 0, 0, 0]);
        assert_eq!(chunks[1].kind, *b"IDAT");
        assert_eq!(chunks[2].kind, *b"IEND");
        assert!(chunks[2].data.is_empty());

        let (raw, blocks) = decode_stored_zlib(chunks[1].data);
        assert_eq!(blocks, 1);
        assert_eq!(raw, [0, 255, 0, 0, 255, 0, 255, 0, 128]);
    }

    #[test]
    fn png_encoding_is_byte_deterministic_and_splits_large_stored_blocks() {
        let rgba = vec![0x5a; 20_000 * 4];
        let first = encode_rgba_png(20_000, 1, &rgba).expect("large row encodes");
        let second = encode_rgba_png(20_000, 1, &rgba).expect("same row encodes again");
        assert_eq!(first, second);

        let chunks = parse_chunks(&first);
        let (_, blocks) = decode_stored_zlib(chunks[1].data);
        assert_eq!(blocks, 2);
    }

    #[test]
    fn comparison_bundle_emits_three_valid_pngs() {
        let native = PixelArtifact::new(1, 1, vec![10, 20, 30, 255]).expect("valid native pixel");
        let baseline =
            PixelArtifact::new(1, 1, vec![8, 20, 31, 250]).expect("valid baseline pixel");
        let bundle = encode_pixel_comparison_pngs(&native, &baseline)
            .expect("matching images produce a comparison bundle");

        assert_eq!(bundle.summary().different_channels(), 3);
        for png in [
            bundle.native_png(),
            bundle.baseline_png(),
            bundle.difference_png(),
        ] {
            assert_eq!(&png[..PNG_SIGNATURE.len()], PNG_SIGNATURE);
            assert_eq!(parse_chunks(png).len(), 3);
        }

        let chunks = parse_chunks(bundle.difference_png());
        let (raw, _) = decode_stored_zlib(chunks[1].data);
        assert_eq!(raw, [0, 2, 0, 1, 255]);
    }

    #[test]
    fn visible_diff_is_opaque_when_rgb_differs_but_alpha_matches() {
        let native = PixelArtifact::new(1, 1, vec![10, 20, 30, 255]).unwrap();
        let baseline = PixelArtifact::new(1, 1, vec![8, 20, 31, 255]).unwrap();
        let bundle = encode_pixel_comparison_pngs(&native, &baseline).unwrap();
        let chunks = parse_chunks(bundle.difference_png());
        let (raw, _) = decode_stored_zlib(chunks[1].data);
        assert_eq!(raw, [0, 2, 0, 1, 255]);
    }

    #[test]
    fn rejects_png_buffer_mismatch_and_dimension_overflow() {
        assert_eq!(
            encode_rgba_png(1, 1, &[0; 3]),
            Err(PngError::Pixel(PixelError::BufferLengthMismatch {
                role: PixelBufferRole::PngInput,
                expected: 4,
                actual: 3,
            }))
        );
        assert_eq!(
            encode_rgba_png(u32::MAX, u32::MAX, &[]),
            Err(PngError::Pixel(PixelError::DimensionOverflow {
                width: u32::MAX,
                height: u32::MAX,
            }))
        );
    }

    fn parse_chunks(png: &[u8]) -> Vec<Chunk<'_>> {
        assert!(png.starts_with(PNG_SIGNATURE));
        let mut chunks = Vec::new();
        let mut offset = PNG_SIGNATURE.len();

        while offset < png.len() {
            assert!(png.len() - offset >= 12);
            let length = u32::from_be_bytes(
                png[offset..offset + 4]
                    .try_into()
                    .expect("four-byte chunk length"),
            ) as usize;
            let kind_start = offset + 4;
            let data_start = kind_start + 4;
            let data_end = data_start + length;
            let crc_end = data_end + 4;
            assert!(crc_end <= png.len());

            let kind: [u8; 4] = png[kind_start..data_start]
                .try_into()
                .expect("four-byte chunk kind");
            let stored_crc = u32::from_be_bytes(
                png[data_end..crc_end]
                    .try_into()
                    .expect("four-byte chunk checksum"),
            );
            assert_eq!(stored_crc, crc32(&png[kind_start..data_end]));
            chunks.push(Chunk {
                kind,
                data: &png[data_start..data_end],
            });
            offset = crc_end;
        }

        assert_eq!(offset, png.len());
        chunks
    }

    fn decode_stored_zlib(zlib: &[u8]) -> (Vec<u8>, usize) {
        assert!(zlib.len() >= 11);
        assert_eq!(zlib[0] & 0x0f, 8);
        assert_eq!(u16::from_be_bytes([zlib[0], zlib[1]]) % 31, 0);
        assert_eq!(zlib[1] & 0x20, 0);

        let mut offset = 2usize;
        let mut raw = Vec::new();
        let mut blocks = 0usize;
        loop {
            let header = zlib[offset];
            offset += 1;
            assert_eq!(header & 0x06, 0);
            let final_block = header & 1 != 0;
            let length = usize::from(u16::from_le_bytes([zlib[offset], zlib[offset + 1]]));
            let complement = u16::from_le_bytes([zlib[offset + 2], zlib[offset + 3]]);
            assert_eq!(!(length as u16), complement);
            offset += 4;
            raw.extend_from_slice(&zlib[offset..offset + length]);
            offset += length;
            blocks += 1;
            if final_block {
                break;
            }
        }

        assert_eq!(zlib.len() - offset, 4);
        let stored_adler = u32::from_be_bytes(
            zlib[offset..offset + 4]
                .try_into()
                .expect("four-byte Adler checksum"),
        );
        assert_eq!(stored_adler, adler32(&raw));
        (raw, blocks)
    }
}
