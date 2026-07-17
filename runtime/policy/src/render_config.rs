use crate::canonical_hash::CanonicalHasher;
use crate::{PolicyError, RenderConfigHash};

const RENDER_CONFIG_SCHEMA_VERSION: u16 = 1;
const MAX_TILE_EDGE: u32 = 4096;
const MAX_TILE_HALO: u16 = 256;
const MAX_CURVE_FLATNESS_DENOMINATOR: u32 = 1_000_000;
const MAX_CURVE_RECURSION: u8 = 64;
const MAX_CANCELLATION_INTERVAL: u32 = 1_000_000;

/// Registered Native product backend.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum NativeBackend {
    /// Independently reviewed deterministic Reference CPU implementation.
    ReferenceCpu = 1,
    /// Product tiled Fast CPU implementation.
    FastCpu = 2,
}

/// Product quality policy.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum QualityPolicy {
    /// Latency-oriented Native preview.
    Preview = 1,
    /// Final registered Native output.
    Full = 2,
}

/// Pixel storage format.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PixelFormat {
    /// Eight bits each of red, green, blue, and alpha.
    Rgba8 = 1,
}

/// Pixel alpha representation.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum AlphaMode {
    /// Straight unassociated alpha.
    Straight = 1,
    /// Premultiplied associated alpha.
    Premultiplied = 2,
}

/// Registered output color profile.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ColorProfile {
    /// Deterministic registered sRGB conversion.
    Srgb = 1,
}

/// Complete registered output profile.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct OutputProfile {
    id: u32,
    color: ColorProfile,
    format: PixelFormat,
    alpha: AlphaMode,
}

impl OutputProfile {
    /// Registered straight-alpha sRGB RGBA8 output.
    pub const SRGB_RGBA8_STRAIGHT: Self = Self {
        id: 1,
        color: ColorProfile::Srgb,
        format: PixelFormat::Rgba8,
        alpha: AlphaMode::Straight,
    };

    /// Returns the stable profile ID.
    pub const fn id(self) -> u32 {
        self.id
    }

    /// Returns the color profile.
    pub const fn color(self) -> ColorProfile {
        self.color
    }

    /// Returns the pixel format.
    pub const fn format(self) -> PixelFormat {
        self.format
    }

    /// Returns the alpha representation.
    pub const fn alpha(self) -> AlphaMode {
        self.alpha
    }

    #[cfg(test)]
    pub(crate) const fn hash_test_variant() -> Self {
        Self {
            id: 2,
            color: ColorProfile::Srgb,
            format: PixelFormat::Rgba8,
            alpha: AlphaMode::Premultiplied,
        }
    }
}

/// Coverage sampling contract.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum AntialiasMode {
    /// One center sample per output pixel.
    SingleSample = 1,
    /// Fixed 4×4 coverage grid.
    Coverage4x4 = 2,
    /// Fixed 8×8 coverage grid.
    Coverage8x8 = 3,
}

/// Image resampling contract.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ImageSampling {
    /// Nearest decoded sample.
    Nearest = 1,
    /// Bilinear interpolation in the registered working space.
    Bilinear = 2,
}

/// Glyph raster sampling contract.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum GlyphSampling {
    /// Rasterize embedded outlines through the same coverage kernel as paths.
    OutlineCoverage = 1,
}

/// Intermediate compositing contract.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CompositingMode {
    /// Premultiplied endpoint-inclusive Q16 working channels.
    PremultipliedQ16 = 1,
}

/// Unvalidated complete Native render configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RenderConfigInput {
    /// Selected Native backend.
    pub backend: NativeBackend,
    /// Quality policy.
    pub quality: QualityPolicy,
    /// Complete output profile.
    pub output_profile: OutputProfile,
    /// Nominal tile width before edge clipping.
    pub tile_width: u32,
    /// Nominal tile height before edge clipping.
    pub tile_height: u32,
    /// Deterministic raster halo around a product tile.
    pub tile_halo: u16,
    /// Coverage sampling contract.
    pub antialias: AntialiasMode,
    /// Reciprocal flatness tolerance in device-space subunits.
    pub curve_flatness_denominator: u32,
    /// Maximum deterministic curve subdivision recursion.
    pub curve_recursion: u8,
    /// Image resampling contract.
    pub image_sampling: ImageSampling,
    /// Glyph sampling contract.
    pub glyph_sampling: GlyphSampling,
    /// Intermediate compositing contract.
    pub compositing: CompositingMode,
    /// Backend loop work interval between cooperative cancellation checks.
    pub cancellation_interval: u32,
}

impl RenderConfigInput {
    /// Registered full-quality Reference CPU configuration.
    pub const fn reference_cpu_full() -> Self {
        Self {
            backend: NativeBackend::ReferenceCpu,
            quality: QualityPolicy::Full,
            output_profile: OutputProfile::SRGB_RGBA8_STRAIGHT,
            tile_width: 256,
            tile_height: 256,
            tile_halo: 2,
            antialias: AntialiasMode::Coverage4x4,
            curve_flatness_denominator: 256,
            curve_recursion: 16,
            image_sampling: ImageSampling::Nearest,
            glyph_sampling: GlyphSampling::OutlineCoverage,
            compositing: CompositingMode::PremultipliedQ16,
            cancellation_interval: 256,
        }
    }

    /// Registered full-quality Fast CPU configuration.
    pub const fn fast_cpu_full() -> Self {
        Self {
            backend: NativeBackend::FastCpu,
            ..Self::reference_cpu_full()
        }
    }
}

/// Validated immutable Native render configuration and its complete typed digest.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RenderConfig {
    input: RenderConfigInput,
    hash: RenderConfigHash,
}

impl RenderConfig {
    /// Validates a complete configuration and computes its domain-separated SHA-256 identity.
    pub fn validate(input: RenderConfigInput) -> Result<Self, PolicyError> {
        let halo_twice = u32::from(input.tile_halo)
            .checked_mul(2)
            .ok_or_else(PolicyError::numeric_overflow)?;
        if input.tile_width == 0
            || input.tile_width > MAX_TILE_EDGE
            || input.tile_height == 0
            || input.tile_height > MAX_TILE_EDGE
            || input.tile_halo > MAX_TILE_HALO
            || halo_twice > input.tile_width
            || halo_twice > input.tile_height
            || input.curve_flatness_denominator == 0
            || input.curve_flatness_denominator > MAX_CURVE_FLATNESS_DENOMINATOR
            || input.curve_recursion == 0
            || input.curve_recursion > MAX_CURVE_RECURSION
            || input.cancellation_interval == 0
            || input.cancellation_interval > MAX_CANCELLATION_INTERVAL
        {
            return Err(PolicyError::invalid_render_config());
        }
        let hash = RenderConfigHash::new(hash_input(input)?);
        Ok(Self { input, hash })
    }

    /// Returns the complete validated input.
    pub const fn input(self) -> RenderConfigInput {
        self.input
    }

    /// Returns the selected Native backend.
    pub const fn backend(self) -> NativeBackend {
        self.input.backend
    }

    /// Returns the quality policy.
    pub const fn quality(self) -> QualityPolicy {
        self.input.quality
    }

    /// Returns the output profile.
    pub const fn output_profile(self) -> OutputProfile {
        self.input.output_profile
    }

    /// Returns the nominal tile dimensions.
    pub const fn tile_size(self) -> (u32, u32) {
        (self.input.tile_width, self.input.tile_height)
    }

    /// Returns the complete typed configuration digest.
    pub const fn hash(self) -> RenderConfigHash {
        self.hash
    }
}

fn hash_input(input: RenderConfigInput) -> Result<[u8; 32], PolicyError> {
    hash_fields(RenderConfigHashFields::from_input(input))
}

#[derive(Clone, Copy)]
struct RenderConfigHashFields {
    backend: u8,
    quality: u8,
    output_profile_id: u32,
    output_color: u8,
    output_format: u8,
    output_alpha: u8,
    tile_width: u32,
    tile_height: u32,
    tile_halo: u16,
    antialias: u8,
    curve_flatness_denominator: u32,
    curve_recursion: u8,
    image_sampling: u8,
    glyph_sampling: u8,
    compositing: u8,
    cancellation_interval: u32,
}

impl RenderConfigHashFields {
    const fn from_input(input: RenderConfigInput) -> Self {
        Self {
            backend: input.backend as u8,
            quality: input.quality as u8,
            output_profile_id: input.output_profile.id(),
            output_color: input.output_profile.color() as u8,
            output_format: input.output_profile.format() as u8,
            output_alpha: input.output_profile.alpha() as u8,
            tile_width: input.tile_width,
            tile_height: input.tile_height,
            tile_halo: input.tile_halo,
            antialias: input.antialias as u8,
            curve_flatness_denominator: input.curve_flatness_denominator,
            curve_recursion: input.curve_recursion,
            image_sampling: input.image_sampling as u8,
            glyph_sampling: input.glyph_sampling as u8,
            compositing: input.compositing as u8,
            cancellation_interval: input.cancellation_interval,
        }
    }
}

fn hash_fields(fields: RenderConfigHashFields) -> Result<[u8; 32], PolicyError> {
    let mut hasher = CanonicalHasher::new(b"render-config/v1");
    hasher.u16(RENDER_CONFIG_SCHEMA_VERSION);
    hasher.u8(fields.backend);
    hasher.u8(fields.quality);
    hasher.u32(fields.output_profile_id);
    hasher.u8(fields.output_color);
    hasher.u8(fields.output_format);
    hasher.u8(fields.output_alpha);
    hasher.u32(fields.tile_width);
    hasher.u32(fields.tile_height);
    hasher.u16(fields.tile_halo);
    hasher.u8(fields.antialias);
    hasher.u32(fields.curve_flatness_denominator);
    hasher.u8(fields.curve_recursion);
    hasher.u8(fields.image_sampling);
    hasher.u8(fields.glyph_sampling);
    hasher.u8(fields.compositing);
    hasher.u32(fields.cancellation_interval);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_CANCELLATION_INTERVAL, MAX_CURVE_FLATNESS_DENOMINATOR, MAX_CURVE_RECURSION,
        MAX_TILE_EDGE, MAX_TILE_HALO, RenderConfig, RenderConfigHashFields, RenderConfigInput,
        hash_fields,
    };
    use crate::PolicyErrorCode;

    #[test]
    fn every_config_field_is_bound_into_the_digest() {
        let input = RenderConfigInput::fast_cpu_full();
        let fields = RenderConfigHashFields::from_input(input);
        let expected = hash_fields(fields).unwrap();
        let variants = [
            RenderConfigHashFields {
                backend: fields.backend + 1,
                ..fields
            },
            RenderConfigHashFields {
                quality: fields.quality + 1,
                ..fields
            },
            RenderConfigHashFields {
                output_profile_id: fields.output_profile_id + 1,
                ..fields
            },
            RenderConfigHashFields {
                output_color: fields.output_color + 1,
                ..fields
            },
            RenderConfigHashFields {
                output_format: fields.output_format + 1,
                ..fields
            },
            RenderConfigHashFields {
                output_alpha: fields.output_alpha + 1,
                ..fields
            },
            RenderConfigHashFields {
                tile_width: fields.tile_width + 1,
                ..fields
            },
            RenderConfigHashFields {
                tile_height: fields.tile_height + 1,
                ..fields
            },
            RenderConfigHashFields {
                tile_halo: fields.tile_halo + 1,
                ..fields
            },
            RenderConfigHashFields {
                antialias: fields.antialias + 1,
                ..fields
            },
            RenderConfigHashFields {
                curve_flatness_denominator: fields.curve_flatness_denominator + 1,
                ..fields
            },
            RenderConfigHashFields {
                curve_recursion: fields.curve_recursion + 1,
                ..fields
            },
            RenderConfigHashFields {
                image_sampling: fields.image_sampling + 1,
                ..fields
            },
            RenderConfigHashFields {
                glyph_sampling: fields.glyph_sampling + 1,
                ..fields
            },
            RenderConfigHashFields {
                compositing: fields.compositing + 1,
                ..fields
            },
            RenderConfigHashFields {
                cancellation_interval: fields.cancellation_interval + 1,
                ..fields
            },
        ];
        for variant in variants {
            assert_ne!(hash_fields(variant).unwrap(), expected);
        }
        assert_eq!(
            RenderConfig::validate(input).unwrap().hash().digest(),
            &expected
        );
        assert_ne!(
            RenderConfig::validate(RenderConfigInput::reference_cpu_full())
                .unwrap()
                .hash()
                .digest(),
            &expected
        );
    }

    #[test]
    fn every_numeric_config_bound_covers_zero_exact_maximum_and_maximum_plus_one() {
        let base = RenderConfigInput::fast_cpu_full();
        let halo_base = RenderConfigInput {
            tile_width: MAX_TILE_EDGE,
            tile_height: MAX_TILE_EDGE,
            ..base
        };
        for valid in [
            RenderConfigInput {
                tile_width: MAX_TILE_EDGE,
                ..base
            },
            RenderConfigInput {
                tile_height: MAX_TILE_EDGE,
                ..base
            },
            RenderConfigInput {
                tile_halo: 0,
                ..base
            },
            RenderConfigInput {
                tile_halo: MAX_TILE_HALO,
                ..halo_base
            },
            RenderConfigInput {
                curve_flatness_denominator: MAX_CURVE_FLATNESS_DENOMINATOR,
                ..base
            },
            RenderConfigInput {
                curve_recursion: MAX_CURVE_RECURSION,
                ..base
            },
            RenderConfigInput {
                cancellation_interval: MAX_CANCELLATION_INTERVAL,
                ..base
            },
        ] {
            assert!(RenderConfig::validate(valid).is_ok());
        }

        for invalid in [
            RenderConfigInput {
                tile_width: 0,
                ..base
            },
            RenderConfigInput {
                tile_width: MAX_TILE_EDGE + 1,
                ..base
            },
            RenderConfigInput {
                tile_height: 0,
                ..base
            },
            RenderConfigInput {
                tile_height: MAX_TILE_EDGE + 1,
                ..base
            },
            RenderConfigInput {
                tile_halo: MAX_TILE_HALO + 1,
                ..halo_base
            },
            RenderConfigInput {
                curve_flatness_denominator: 0,
                ..base
            },
            RenderConfigInput {
                curve_flatness_denominator: MAX_CURVE_FLATNESS_DENOMINATOR + 1,
                ..base
            },
            RenderConfigInput {
                curve_recursion: 0,
                ..base
            },
            RenderConfigInput {
                curve_recursion: MAX_CURVE_RECURSION + 1,
                ..base
            },
            RenderConfigInput {
                cancellation_interval: 0,
                ..base
            },
            RenderConfigInput {
                cancellation_interval: MAX_CANCELLATION_INTERVAL + 1,
                ..base
            },
        ] {
            let error = RenderConfig::validate(invalid).unwrap_err();
            assert_eq!(error.code(), PolicyErrorCode::InvalidRenderConfig);
        }

        let exact_halo_fit = RenderConfigInput {
            tile_width: 4,
            tile_height: 4,
            tile_halo: 2,
            ..base
        };
        assert!(RenderConfig::validate(exact_halo_fit).is_ok());
        assert_eq!(
            RenderConfig::validate(RenderConfigInput {
                tile_halo: 3,
                ..exact_halo_fit
            })
            .unwrap_err()
            .code(),
            PolicyErrorCode::InvalidRenderConfig
        );
    }
}
