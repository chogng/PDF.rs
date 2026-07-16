//! Deterministic Scene device-color conversion and premultiplied-alpha compositing.
//!
//! `reference-color-v1` identifies the conversion, premultiplication, blend, and
//! rounding algorithms in this module. It is deliberately independent from the
//! published pixel-buffer encoding profile (`sRGB-reference-v1`).
//!
//! Every production kernel operates on a fixed number of integer channels. The
//! module performs no allocation, recursion, unbounded traversal, or external
//! calls, so callers account one fixed color/compositing work unit per invocation
//! rather than lending a child fuel or cancellation scope.

use pdf_rs_scene::{BlendMode as SceneBlendMode, DeviceColor, Paint as ScenePaint, SceneUnit};

const Q16_SCALE_U32: u32 = 1 << 16;
const Q16_SCALE_U64: u64 = 1 << 16;
const Q16_HALF_U64: u64 = Q16_SCALE_U64 / 2;

/// The Scene device-color type consumed by the Reference color kernel.
///
/// This alias intentionally avoids a second public color model with independent
/// channel widths or validation rules.
pub type ReferenceDeviceColor = DeviceColor;

/// The exact integer representation for one normalized endpoint-inclusive Q16 value.
///
/// Values are bounded to `[0, 65_536]`; zero represents `0.0` and `65_536`
/// represents `1.0`. Product and quotient conversions round to nearest, with an
/// exact half rounded toward positive infinity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NormalizedQ16(u32);

impl NormalizedQ16 {
    /// Zero intensity or opacity.
    pub const ZERO: Self = Self(0);

    /// Full intensity or opacity.
    pub const ONE: Self = Self(Q16_SCALE_U32);

    /// Creates a bounded normalized Q16 value from its exact integer representation.
    pub const fn from_bits(bits: u32) -> Option<Self> {
        if bits <= Q16_SCALE_U32 {
            Some(Self(bits))
        } else {
            None
        }
    }

    /// Returns the exact integer representation in `[0, 65_536]`.
    pub const fn bits(self) -> u32 {
        self.0
    }

    const fn from_endpoint_u16(value: u16) -> Self {
        let numerator = value as u64 * Q16_SCALE_U64;
        Self(((numerator + (u16::MAX as u64 / 2)) / u16::MAX as u64) as u32)
    }

    const fn to_u8(self) -> u8 {
        (((self.0 as u64 * u8::MAX as u64) + Q16_HALF_U64) / Q16_SCALE_U64) as u8
    }

    const fn complement(self) -> Self {
        Self(Q16_SCALE_U32 - self.0)
    }

    fn multiply(self, other: Self) -> Self {
        round_q16(u64::from(self.0) * u64::from(other.0))
    }
}

impl From<SceneUnit> for NormalizedQ16 {
    fn from(value: SceneUnit) -> Self {
        Self::from_endpoint_u16(value.get())
    }
}

/// Versioned deterministic device-color conversion and compositing algorithm.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ReferenceColorProfile {
    /// Frozen M3 conversion, premultiplication, blend, and rounding rules.
    ReferenceColorV1,
}

impl ReferenceColorProfile {
    /// Returns the stable algorithm identity, independent from output encoding.
    pub const fn label(self) -> &'static str {
        match self {
            Self::ReferenceColorV1 => "reference-color-v1",
        }
    }

    /// Converts one validated Scene Device color to straight project-sRGB Q16.
    ///
    /// DeviceGray is replicated to RGB and DeviceRGB is transferred channel for
    /// channel. DeviceCMYK uses the frozen additive-black rule
    /// `RGB = 1 - min(1, CMY + K)` independently for each output channel.
    pub fn convert(self, color: ReferenceDeviceColor) -> ReferenceSrgbQ16 {
        match self {
            Self::ReferenceColorV1 => convert_reference_color_v1(color),
        }
    }

    /// Converts one complete Scene paint into a premultiplied source and blend mode.
    ///
    /// This is the bounded Scene-to-raster adapter. It performs a fixed number of
    /// channel operations and introduces no additional public paint model. Callers
    /// must reject Scene capability requirements marked unsupported before invoking
    /// this adapter; unsupported color spaces, masks, groups, and blend modes are
    /// never represented by a fabricated fallback `Paint`.
    pub fn prepare_paint(self, paint: ScenePaint) -> (PremultipliedRgbaQ16, ReferenceBlendMode) {
        (
            self.convert(paint.color())
                .with_constant_alpha(paint.alpha().into()),
            paint.blend_mode().into(),
        )
    }
}

/// Straight project-sRGB color in normalized Q16 channels.
///
/// This names the deterministic working color space, not the published pixel
/// buffer's byte encoding or row layout.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ReferenceSrgbQ16 {
    red: NormalizedQ16,
    green: NormalizedQ16,
    blue: NormalizedQ16,
}

impl ReferenceSrgbQ16 {
    /// Creates one straight working-space color from bounded channels.
    pub const fn new(red: NormalizedQ16, green: NormalizedQ16, blue: NormalizedQ16) -> Self {
        Self { red, green, blue }
    }

    /// Creates one neutral working-space color.
    pub const fn gray(value: NormalizedQ16) -> Self {
        Self::new(value, value, value)
    }

    /// Returns the straight red channel.
    pub const fn red(self) -> NormalizedQ16 {
        self.red
    }

    /// Returns the straight green channel.
    pub const fn green(self) -> NormalizedQ16 {
        self.green
    }

    /// Returns the straight blue channel.
    pub const fn blue(self) -> NormalizedQ16 {
        self.blue
    }

    /// Applies one constant alpha with one rounded Q16 product per channel.
    pub fn with_constant_alpha(self, alpha: NormalizedQ16) -> PremultipliedRgbaQ16 {
        PremultipliedRgbaQ16 {
            red: self.red.multiply(alpha),
            green: self.green.multiply(alpha),
            blue: self.blue.multiply(alpha),
            alpha,
        }
    }
}

/// One premultiplied project-sRGB pixel in normalized Q16 channels.
///
/// Construction preserves `color <= alpha` for every channel. Hidden color is
/// therefore canonicalized away when alpha is zero.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PremultipliedRgbaQ16 {
    red: NormalizedQ16,
    green: NormalizedQ16,
    blue: NormalizedQ16,
    alpha: NormalizedQ16,
}

impl PremultipliedRgbaQ16 {
    /// Fully transparent canonical black.
    pub const TRANSPARENT: Self = Self {
        red: NormalizedQ16::ZERO,
        green: NormalizedQ16::ZERO,
        blue: NormalizedQ16::ZERO,
        alpha: NormalizedQ16::ZERO,
    };

    /// Creates premultiplied RGBA when all color channels are at most alpha.
    pub const fn new(
        red: NormalizedQ16,
        green: NormalizedQ16,
        blue: NormalizedQ16,
        alpha: NormalizedQ16,
    ) -> Option<Self> {
        if red.0 <= alpha.0 && green.0 <= alpha.0 && blue.0 <= alpha.0 {
            Some(Self {
                red,
                green,
                blue,
                alpha,
            })
        } else {
            None
        }
    }

    /// Returns the premultiplied red channel.
    pub const fn red(self) -> NormalizedQ16 {
        self.red
    }

    /// Returns the premultiplied green channel.
    pub const fn green(self) -> NormalizedQ16 {
        self.green
    }

    /// Returns the premultiplied blue channel.
    pub const fn blue(self) -> NormalizedQ16 {
        self.blue
    }

    /// Returns the alpha channel.
    pub const fn alpha(self) -> NormalizedQ16 {
        self.alpha
    }

    /// Applies another constant alpha with one rounded Q16 product per channel.
    pub fn apply_constant_alpha(self, alpha: NormalizedQ16) -> Self {
        Self {
            red: self.red.multiply(alpha),
            green: self.green.multiply(alpha),
            blue: self.blue.multiply(alpha),
            alpha: self.alpha.multiply(alpha),
        }
    }

    /// Publishes straight-alpha RGBA8 with fixed two-stage rounding.
    ///
    /// Nonzero color channels are first unpremultiplied to normalized Q16, then
    /// every channel is independently rounded to normalized eight-bit.
    /// Transparent input publishes canonical transparent black.
    pub fn to_straight_rgba8(self) -> [u8; 4] {
        if self.alpha == NormalizedQ16::ZERO {
            return [0, 0, 0, 0];
        }
        [
            unpremultiply(self.red, self.alpha).to_u8(),
            unpremultiply(self.green, self.alpha).to_u8(),
            unpremultiply(self.blue, self.alpha).to_u8(),
            self.alpha.to_u8(),
        ]
    }
}

/// Basic separable blend mode for deterministic source-over compositing.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ReferenceBlendMode {
    /// Source color replaces backdrop color in their overlap.
    Normal,
    /// Source and backdrop channels are multiplied in their overlap.
    Multiply,
    /// Source and backdrop channels use the inverse-product Screen rule.
    Screen,
}

impl From<SceneBlendMode> for ReferenceBlendMode {
    fn from(value: SceneBlendMode) -> Self {
        match value {
            SceneBlendMode::Normal => Self::Normal,
            SceneBlendMode::Multiply => Self::Multiply,
            SceneBlendMode::Screen => Self::Screen,
        }
    }
}

impl ReferenceBlendMode {
    /// Composites `source` over `backdrop` in premultiplied Q16.
    ///
    /// Each output channel builds its complete nonnegative Q32 numerator and
    /// rounds exactly once back to Q16. Exact half values round toward positive
    /// infinity. The operation is an O(1), allocation-free single-pixel kernel.
    pub fn source_over(
        self,
        source: PremultipliedRgbaQ16,
        backdrop: PremultipliedRgbaQ16,
    ) -> PremultipliedRgbaQ16 {
        let source_alpha = u64::from(source.alpha.0);
        let backdrop_alpha = u64::from(backdrop.alpha.0);
        let alpha = round_q16(
            source_alpha * Q16_SCALE_U64 + backdrop_alpha * (Q16_SCALE_U64 - source_alpha),
        );

        let result = PremultipliedRgbaQ16 {
            red: composite_channel(self, source.red, source.alpha, backdrop.red, backdrop.alpha),
            green: composite_channel(
                self,
                source.green,
                source.alpha,
                backdrop.green,
                backdrop.alpha,
            ),
            blue: composite_channel(
                self,
                source.blue,
                source.alpha,
                backdrop.blue,
                backdrop.alpha,
            ),
            alpha,
        };
        debug_assert!(result.red.0 <= result.alpha.0);
        debug_assert!(result.green.0 <= result.alpha.0);
        debug_assert!(result.blue.0 <= result.alpha.0);
        result
    }
}

fn convert_reference_color_v1(color: DeviceColor) -> ReferenceSrgbQ16 {
    match color {
        DeviceColor::Gray(gray) => ReferenceSrgbQ16::gray(gray.into()),
        DeviceColor::Rgb { red, green, blue } => {
            ReferenceSrgbQ16::new(red.into(), green.into(), blue.into())
        }
        DeviceColor::Cmyk {
            cyan,
            magenta,
            yellow,
            black,
        } => cmyk_to_srgb(cyan.into(), magenta.into(), yellow.into(), black.into()),
    }
}

fn composite_channel(
    mode: ReferenceBlendMode,
    source: NormalizedQ16,
    source_alpha: NormalizedQ16,
    backdrop: NormalizedQ16,
    backdrop_alpha: NormalizedQ16,
) -> NormalizedQ16 {
    let source = u64::from(source.0);
    let source_alpha = u64::from(source_alpha.0);
    let backdrop = u64::from(backdrop.0);
    let backdrop_alpha = u64::from(backdrop_alpha.0);
    let numerator = match mode {
        ReferenceBlendMode::Normal => {
            source * Q16_SCALE_U64 + backdrop * (Q16_SCALE_U64 - source_alpha)
        }
        ReferenceBlendMode::Multiply => {
            backdrop * (Q16_SCALE_U64 - source_alpha)
                + source * (Q16_SCALE_U64 - backdrop_alpha)
                + source * backdrop
        }
        ReferenceBlendMode::Screen => {
            source * Q16_SCALE_U64 + backdrop * Q16_SCALE_U64 - source * backdrop
        }
    };
    round_q16(numerator)
}

fn cmyk_to_srgb(
    cyan: NormalizedQ16,
    magenta: NormalizedQ16,
    yellow: NormalizedQ16,
    black: NormalizedQ16,
) -> ReferenceSrgbQ16 {
    let remove = |component: NormalizedQ16| {
        NormalizedQ16(component.0.saturating_add(black.0).min(Q16_SCALE_U32)).complement()
    };
    ReferenceSrgbQ16::new(remove(cyan), remove(magenta), remove(yellow))
}

fn unpremultiply(channel: NormalizedQ16, alpha: NormalizedQ16) -> NormalizedQ16 {
    debug_assert!(alpha != NormalizedQ16::ZERO);
    debug_assert!(channel.0 <= alpha.0);
    let numerator = u64::from(channel.0) * Q16_SCALE_U64;
    let denominator = u64::from(alpha.0);
    let rounded = (numerator + denominator / 2) / denominator;
    debug_assert!(rounded <= Q16_SCALE_U64);
    NormalizedQ16(rounded as u32)
}

fn round_q16(numerator: u64) -> NormalizedQ16 {
    let rounded = (numerator + Q16_HALF_U64) / Q16_SCALE_U64;
    debug_assert!(rounded <= Q16_SCALE_U64);
    NormalizedQ16(rounded as u32)
}
