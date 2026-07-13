use std::error::Error;
use std::fmt;

use crate::json::{CanonicalJson, push_hex, push_number, push_u8_array};

const RGBA_CHANNELS: usize = 4;
const PIXEL_SCHEMA: u32 = 1;

/// Identifies the buffer whose byte length failed validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PixelBufferRole {
    /// A buffer passed to [`PixelArtifact::new`].
    Artifact,
    /// Raw input passed to the PNG encoder.
    PngInput,
}

impl fmt::Display for PixelBufferRole {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Artifact => formatter.write_str("artifact"),
            Self::PngInput => formatter.write_str("PNG input"),
        }
    }
}

/// Validation or exact-comparison failure for RGBA artifacts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PixelError {
    /// PNG and canonical pixel artifacts require non-zero dimensions.
    ZeroDimension {
        /// Declared image width.
        width: u32,
        /// Declared image height.
        height: u32,
    },
    /// `width * height * 4` could not be represented by the host.
    DimensionOverflow {
        /// Declared image width.
        width: u32,
        /// Declared image height.
        height: u32,
    },
    /// A buffer does not contain exactly four bytes per pixel.
    BufferLengthMismatch {
        /// Semantic role of the rejected buffer.
        role: PixelBufferRole,
        /// Required RGBA byte length.
        expected: usize,
        /// Supplied RGBA byte length.
        actual: usize,
    },
    /// Native and baseline images have different dimensions.
    DimensionMismatch {
        /// Native image width.
        native_width: u32,
        /// Native image height.
        native_height: u32,
        /// Baseline image width.
        baseline_width: u32,
        /// Baseline image height.
        baseline_height: u32,
    },
    /// An exact diff counter overflowed.
    ComparisonOverflow,
    /// The tool could not reserve the requested deterministic output buffer.
    AllocationFailed {
        /// Requested deterministic output capacity.
        requested: usize,
    },
}

impl fmt::Display for PixelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroDimension { width, height } => {
                write!(
                    formatter,
                    "pixel dimensions must be non-zero, got {width}x{height}"
                )
            }
            Self::DimensionOverflow { width, height } => write!(
                formatter,
                "RGBA byte length overflows for dimensions {width}x{height}"
            ),
            Self::BufferLengthMismatch {
                role,
                expected,
                actual,
            } => write!(
                formatter,
                "{role} RGBA length mismatch: expected {expected} bytes, got {actual}"
            ),
            Self::DimensionMismatch {
                native_width,
                native_height,
                baseline_width,
                baseline_height,
            } => write!(
                formatter,
                "pixel dimensions differ: native {native_width}x{native_height}, baseline {baseline_width}x{baseline_height}"
            ),
            Self::ComparisonOverflow => formatter.write_str("pixel comparison counter overflowed"),
            Self::AllocationFailed { requested } => write!(
                formatter,
                "could not reserve {requested} bytes for pixel comparison output"
            ),
        }
    }
}

impl Error for PixelError {}

/// Validated row-major, eight-bit RGBA output.
#[derive(Clone, Eq, PartialEq)]
pub struct PixelArtifact {
    schema: u32,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl fmt::Debug for PixelArtifact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PixelArtifact")
            .field("schema", &self.schema)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("rgba_bytes", &self.rgba.len())
            .field("content", &"[REDACTED]")
            .finish()
    }
}

impl PixelArtifact {
    /// Validates and creates a canonical RGBA artifact.
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Result<Self, PixelError> {
        validate_rgba_len(width, height, rgba.len(), PixelBufferRole::Artifact)?;
        Ok(Self {
            schema: PIXEL_SCHEMA,
            width,
            height,
            rgba,
        })
    }

    /// Returns the artifact schema version.
    pub const fn schema(&self) -> u32 {
        self.schema
    }

    /// Returns image width in pixels.
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Returns image height in pixels.
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Returns validated row-major RGBA bytes.
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }
}

impl CanonicalJson for PixelArtifact {
    fn write_canonical_json(&self, output: &mut String) {
        output.push_str("{\"height\":");
        push_number(output, self.height);
        output.push_str(",\"rgba_hex\":");
        push_hex(output, &self.rgba);
        output.push_str(",\"schema\":");
        push_number(output, self.schema);
        output.push_str(",\"width\":");
        push_number(output, self.width);
        output.push('}');
    }
}

/// Exact RGBA difference statistics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PixelDiffSummary {
    width: u32,
    height: u32,
    different_pixels: usize,
    different_channels: usize,
    max_channel_delta: [u8; RGBA_CHANNELS],
    total_absolute_delta: u64,
}

impl PixelDiffSummary {
    /// Returns image width in pixels.
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Returns image height in pixels.
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Returns the number of pixels with at least one different channel.
    pub const fn different_pixels(&self) -> usize {
        self.different_pixels
    }

    /// Returns the total number of different RGBA channels.
    pub const fn different_channels(&self) -> usize {
        self.different_channels
    }

    /// Returns the greatest absolute delta observed for R, G, B, and A.
    pub const fn max_channel_delta(&self) -> [u8; RGBA_CHANNELS] {
        self.max_channel_delta
    }

    /// Returns the sum of all absolute channel deltas.
    pub const fn total_absolute_delta(&self) -> u64 {
        self.total_absolute_delta
    }

    /// Reports whether all RGBA channels are exactly equal.
    pub const fn is_exact(&self) -> bool {
        self.different_channels == 0
    }
}

impl CanonicalJson for PixelDiffSummary {
    fn write_canonical_json(&self, output: &mut String) {
        output.push_str("{\"different_channels\":");
        push_number(output, self.different_channels);
        output.push_str(",\"different_pixels\":");
        push_number(output, self.different_pixels);
        output.push_str(",\"exact\":");
        output.push_str(if self.is_exact() { "true" } else { "false" });
        output.push_str(",\"height\":");
        push_number(output, self.height);
        output.push_str(",\"max_channel_delta\":");
        push_u8_array(output, &self.max_channel_delta);
        output.push_str(",\"total_absolute_delta\":");
        push_number(output, self.total_absolute_delta);
        output.push_str(",\"width\":");
        push_number(output, self.width);
        output.push('}');
    }
}

/// Exact RGBA comparison and its absolute per-channel difference image.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PixelDiff {
    summary: PixelDiffSummary,
    absolute_difference: PixelArtifact,
}

impl PixelDiff {
    /// Returns exact aggregate statistics.
    pub const fn summary(&self) -> &PixelDiffSummary {
        &self.summary
    }

    /// Returns an RGBA image containing per-channel absolute deltas.
    pub const fn absolute_difference(&self) -> &PixelArtifact {
        &self.absolute_difference
    }
}

/// Compares validated native and baseline RGBA images exactly.
pub fn compare_pixels(
    native: &PixelArtifact,
    baseline: &PixelArtifact,
) -> Result<PixelDiff, PixelError> {
    if native.width != baseline.width || native.height != baseline.height {
        return Err(PixelError::DimensionMismatch {
            native_width: native.width,
            native_height: native.height,
            baseline_width: baseline.width,
            baseline_height: baseline.height,
        });
    }

    let mut difference = Vec::new();
    difference
        .try_reserve_exact(native.rgba.len())
        .map_err(|_| PixelError::AllocationFailed {
            requested: native.rgba.len(),
        })?;

    let mut different_pixels = 0usize;
    let mut different_channels = 0usize;
    let mut max_channel_delta = [0u8; RGBA_CHANNELS];
    let mut total_absolute_delta = 0u64;

    for (native_pixel, baseline_pixel) in native
        .rgba
        .chunks_exact(RGBA_CHANNELS)
        .zip(baseline.rgba.chunks_exact(RGBA_CHANNELS))
    {
        let mut pixel_differs = false;
        for channel in 0..RGBA_CHANNELS {
            let delta = native_pixel[channel].abs_diff(baseline_pixel[channel]);
            difference.push(delta);
            if delta != 0 {
                pixel_differs = true;
                different_channels = different_channels
                    .checked_add(1)
                    .ok_or(PixelError::ComparisonOverflow)?;
                max_channel_delta[channel] = max_channel_delta[channel].max(delta);
                total_absolute_delta = total_absolute_delta
                    .checked_add(u64::from(delta))
                    .ok_or(PixelError::ComparisonOverflow)?;
            }
        }
        if pixel_differs {
            different_pixels = different_pixels
                .checked_add(1)
                .ok_or(PixelError::ComparisonOverflow)?;
        }
    }

    let absolute_difference = PixelArtifact::new(native.width, native.height, difference)?;
    Ok(PixelDiff {
        summary: PixelDiffSummary {
            width: native.width,
            height: native.height,
            different_pixels,
            different_channels,
            max_channel_delta,
            total_absolute_delta,
        },
        absolute_difference,
    })
}

pub(crate) fn validate_rgba_len(
    width: u32,
    height: u32,
    actual: usize,
    role: PixelBufferRole,
) -> Result<usize, PixelError> {
    let expected = expected_rgba_len(width, height)?;
    if actual != expected {
        return Err(PixelError::BufferLengthMismatch {
            role,
            expected,
            actual,
        });
    }
    Ok(expected)
}

fn expected_rgba_len(width: u32, height: u32) -> Result<usize, PixelError> {
    if width == 0 || height == 0 {
        return Err(PixelError::ZeroDimension { width, height });
    }

    let width =
        usize::try_from(width).map_err(|_| PixelError::DimensionOverflow { width, height })?;
    let height = usize::try_from(height).map_err(|_| PixelError::DimensionOverflow {
        width: u32::try_from(width).unwrap_or(u32::MAX),
        height,
    })?;

    width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(RGBA_CHANNELS))
        .ok_or(PixelError::DimensionOverflow {
            width: u32::try_from(width).unwrap_or(u32::MAX),
            height: u32::try_from(height).unwrap_or(u32::MAX),
        })
}

#[cfg(test)]
mod tests {
    use super::{PixelArtifact, PixelBufferRole, PixelError, compare_pixels};
    use crate::CanonicalJson;

    #[test]
    fn pixel_artifact_serializes_bytes_as_canonical_hex() {
        let artifact =
            PixelArtifact::new(1, 1, vec![0, 15, 16, 255]).expect("one complete pixel is valid");
        assert_eq!(
            artifact.to_canonical_json(),
            "{\"height\":1,\"rgba_hex\":\"000f10ff\",\"schema\":1,\"width\":1}"
        );
    }

    #[test]
    fn exact_pixel_diff_has_zero_summary_and_zero_image() {
        let image =
            PixelArtifact::new(1, 1, vec![1, 2, 3, 4]).expect("one complete pixel is valid");
        let diff = compare_pixels(&image, &image).expect("matching dimensions compare");
        assert!(diff.summary().is_exact());
        assert_eq!(diff.absolute_difference().rgba(), &[0, 0, 0, 0]);
    }

    #[test]
    fn pixel_diff_counts_rgba_channels_and_absolute_deltas() {
        let native = PixelArtifact::new(2, 1, vec![10, 20, 30, 40, 0, 1, 2, 3])
            .expect("two complete pixels are valid");
        let baseline = PixelArtifact::new(2, 1, vec![8, 20, 35, 30, 0, 4, 2, 9])
            .expect("two complete pixels are valid");
        let diff = compare_pixels(&native, &baseline).expect("matching dimensions compare");

        assert_eq!(diff.summary().different_pixels(), 2);
        assert_eq!(diff.summary().different_channels(), 5);
        assert_eq!(diff.summary().max_channel_delta(), [2, 3, 5, 10]);
        assert_eq!(diff.summary().total_absolute_delta(), 26);
        assert_eq!(
            diff.absolute_difference().rgba(),
            &[2, 0, 5, 10, 0, 3, 0, 6]
        );
    }

    #[test]
    fn rejects_buffer_mismatch_zero_dimensions_and_overflow() {
        assert_eq!(
            PixelArtifact::new(1, 1, vec![0; 3]),
            Err(PixelError::BufferLengthMismatch {
                role: PixelBufferRole::Artifact,
                expected: 4,
                actual: 3,
            })
        );
        assert_eq!(
            PixelArtifact::new(0, 1, Vec::new()),
            Err(PixelError::ZeroDimension {
                width: 0,
                height: 1,
            })
        );
        assert_eq!(
            PixelArtifact::new(u32::MAX, u32::MAX, Vec::new()),
            Err(PixelError::DimensionOverflow {
                width: u32::MAX,
                height: u32::MAX,
            })
        );
    }

    #[test]
    fn rejects_dimension_mismatch_before_comparison() {
        let native = PixelArtifact::new(1, 1, vec![0; 4]).expect("valid native image");
        let baseline = PixelArtifact::new(2, 1, vec![0; 8]).expect("valid baseline image");
        assert!(matches!(
            compare_pixels(&native, &baseline),
            Err(PixelError::DimensionMismatch { .. })
        ));
    }

    #[test]
    fn pixel_debug_output_redacts_rgba_bytes() {
        let artifact = PixelArtifact::new(1, 1, vec![17, 34, 51, 68]).unwrap();
        let debug = format!("{artifact:?}");
        assert!(!debug.contains("17, 34, 51, 68"));
        assert!(debug.contains("rgba_bytes: 4"));
        assert!(debug.contains("[REDACTED]"));
    }
}
