/// One straight-alpha sRGBA8 color.
///
/// The RGB channels are deliberately retained even when alpha is zero. This
/// makes `Color` suitable for image pixels as well as constant paint state;
/// compositing canonicalizes a fully transparent result to [`Color::TRANSPARENT`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Color {
    red: u8,
    green: u8,
    blue: u8,
    alpha: u8,
}

impl Color {
    /// Fully transparent black.
    pub const TRANSPARENT: Self = Self::rgba(0, 0, 0, 0);

    /// Opaque black.
    pub const BLACK: Self = Self::rgb(0, 0, 0);

    /// Opaque white.
    pub const WHITE: Self = Self::rgb(u8::MAX, u8::MAX, u8::MAX);

    /// Opaque red.
    pub const RED: Self = Self::rgb(u8::MAX, 0, 0);

    /// Opaque green.
    pub const GREEN: Self = Self::rgb(0, u8::MAX, 0);

    /// Opaque blue.
    pub const BLUE: Self = Self::rgb(0, 0, u8::MAX);

    /// Creates an opaque sRGBA8 color.
    pub const fn rgb(red: u8, green: u8, blue: u8) -> Self {
        Self::rgba(red, green, blue, u8::MAX)
    }

    /// Creates a straight-alpha sRGBA8 color.
    pub const fn rgba(red: u8, green: u8, blue: u8, alpha: u8) -> Self {
        Self {
            red,
            green,
            blue,
            alpha,
        }
    }

    /// Decodes an `0xAARRGGBB` color value.
    pub const fn from_argb(value: u32) -> Self {
        Self::rgba(
            (value >> 16) as u8,
            (value >> 8) as u8,
            value as u8,
            (value >> 24) as u8,
        )
    }

    /// Decodes an `0xRRGGBBAA` color value.
    pub const fn from_rgba_u32(value: u32) -> Self {
        Self::rgba(
            (value >> 24) as u8,
            (value >> 16) as u8,
            (value >> 8) as u8,
            value as u8,
        )
    }

    /// Returns the red channel.
    pub const fn red(self) -> u8 {
        self.red
    }

    /// Returns the green channel.
    pub const fn green(self) -> u8 {
        self.green
    }

    /// Returns the blue channel.
    pub const fn blue(self) -> u8 {
        self.blue
    }

    /// Returns the alpha channel.
    pub const fn alpha(self) -> u8 {
        self.alpha
    }

    /// Returns channels in top-level RGBA order.
    pub const fn channels(self) -> [u8; 4] {
        [self.red, self.green, self.blue, self.alpha]
    }

    /// Returns the color as `0xAARRGGBB`.
    pub const fn argb(self) -> u32 {
        ((self.alpha as u32) << 24)
            | ((self.red as u32) << 16)
            | ((self.green as u32) << 8)
            | self.blue as u32
    }

    /// Returns the color as `0xRRGGBBAA`.
    pub const fn rgba_u32(self) -> u32 {
        ((self.red as u32) << 24)
            | ((self.green as u32) << 16)
            | ((self.blue as u32) << 8)
            | self.alpha as u32
    }

    /// Replaces the alpha channel without changing the RGB channels.
    pub const fn with_alpha(self, alpha: u8) -> Self {
        Self::rgba(self.red, self.green, self.blue, alpha)
    }

    /// Multiplies the alpha channel by an 8-bit opacity factor.
    pub fn with_opacity(self, opacity: u8) -> Self {
        self.with_alpha(to_u8(mul_255(u32::from(self.alpha), u32::from(opacity))))
    }

    /// Returns whether alpha is fully opaque.
    pub const fn is_opaque(self) -> bool {
        self.alpha == u8::MAX
    }

    /// Returns whether alpha is fully transparent.
    pub const fn is_transparent(self) -> bool {
        self.alpha == 0
    }

    /// Composites `self` over `destination` using `blend_mode`.
    pub fn composite(self, destination: Self, blend_mode: BlendMode) -> Self {
        blend_mode.composite(self, destination)
    }
}

/// Compositing operation for source and destination pixels.
///
/// Names use fully spelled-out source and destination terms.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BlendMode {
    /// Clears both source and destination.
    Clear,
    /// Replaces the destination with the source.
    Source,
    /// Preserves the destination.
    Destination,
    /// Standard source-over compositing.
    SourceOver,
    /// Destination-over compositing.
    DestinationOver,
    /// Keeps source covered by destination alpha.
    SourceIn,
    /// Keeps destination covered by source alpha.
    DestinationIn,
    /// Keeps source outside destination alpha.
    SourceOut,
    /// Keeps destination outside source alpha.
    DestinationOut,
    /// Keeps source atop destination.
    SourceAtop,
    /// Keeps destination atop source.
    DestinationAtop,
    /// Keeps pixels covered by exactly one input.
    Xor,
    /// Adds premultiplied components with saturation.
    Plus,
    /// Multiplies premultiplied source and destination components.
    Modulate,
    /// Multiplies source and destination colors.
    Multiply,
    /// Screens source and destination colors.
    Screen,
    /// Uses the destination to select multiply or screen.
    Overlay,
    /// Selects the darker source/destination color per channel.
    Darken,
    /// Selects the lighter source/destination color per channel.
    Lighten,
    /// Brightens the destination to reflect the source.
    ColorDodge,
    /// Darkens the destination to reflect the source.
    ColorBurn,
    /// Uses the source to select multiply or screen.
    HardLight,
    /// Applies the soft-light contrast curve.
    SoftLight,
    /// Uses the absolute channel difference.
    Difference,
    /// Uses a reduced channel difference.
    Exclusion,
    /// Takes hue from source and saturation/luminance from destination.
    Hue,
    /// Takes saturation from source and hue/luminance from destination.
    Saturation,
    /// Takes hue/saturation from source and luminance from destination.
    Color,
    /// Takes luminance from source and hue/saturation from destination.
    Luminosity,
}

impl BlendMode {
    /// Returns whether this is a Porter-Duff operation.
    pub const fn is_porter_duff(self) -> bool {
        matches!(
            self,
            Self::Clear
                | Self::Source
                | Self::Destination
                | Self::SourceOver
                | Self::DestinationOver
                | Self::SourceIn
                | Self::DestinationIn
                | Self::SourceOut
                | Self::DestinationOut
                | Self::SourceAtop
                | Self::DestinationAtop
                | Self::Xor
                | Self::Plus
                | Self::Modulate
        )
    }

    /// Composites `source` over `destination`.
    ///
    /// Color values use straight alpha at the API boundary. Calculations use
    /// rounded premultiplied 8-bit values, and transparent results are
    /// canonicalized to transparent black.
    pub fn composite(self, source: Color, destination: Color) -> Color {
        if matches!(self, Self::SourceOver) && destination.is_transparent() {
            return source.canonicalized();
        }
        if matches!(self, Self::SourceOver) && source.is_transparent() {
            return destination.canonicalized();
        }
        match self {
            Self::Clear => Color::TRANSPARENT,
            Self::Plus => plus(source, destination),
            Self::Modulate => modulate(source, destination),
            Self::Source => source.canonicalized(),
            Self::Destination => destination.canonicalized(),
            Self::SourceOver => {
                porter_duff(source, destination, 255, 255 - u32::from(source.alpha))
            }
            Self::DestinationOver => {
                porter_duff(source, destination, 255 - u32::from(destination.alpha), 255)
            }
            Self::SourceIn => porter_duff(source, destination, u32::from(destination.alpha), 0),
            Self::DestinationIn => porter_duff(source, destination, 0, u32::from(source.alpha)),
            Self::SourceOut => {
                porter_duff(source, destination, 255 - u32::from(destination.alpha), 0)
            }
            Self::DestinationOut => {
                porter_duff(source, destination, 0, 255 - u32::from(source.alpha))
            }
            Self::SourceAtop => porter_duff(
                source,
                destination,
                u32::from(destination.alpha),
                255 - u32::from(source.alpha),
            ),
            Self::DestinationAtop => porter_duff(
                source,
                destination,
                255 - u32::from(destination.alpha),
                u32::from(source.alpha),
            ),
            Self::Xor => porter_duff(
                source,
                destination,
                255 - u32::from(destination.alpha),
                255 - u32::from(source.alpha),
            ),
            Self::Multiply => separable(source, destination, multiply),
            Self::Screen => separable(source, destination, screen),
            Self::Overlay => separable(source, destination, overlay),
            Self::Darken => separable(source, destination, |source, destination| {
                source.min(destination)
            }),
            Self::Lighten => separable(source, destination, |source, destination| {
                source.max(destination)
            }),
            Self::ColorDodge => separable(source, destination, color_dodge),
            Self::ColorBurn => separable(source, destination, color_burn),
            Self::HardLight => separable(source, destination, |source, destination| {
                overlay(destination, source)
            }),
            Self::SoftLight => separable(source, destination, soft_light),
            Self::Difference => separable(source, destination, |source, destination| {
                source.abs_diff(destination)
            }),
            Self::Exclusion => separable(source, destination, exclusion),
            Self::Hue => non_separable(source, destination, hue),
            Self::Saturation => non_separable(source, destination, saturation),
            Self::Color => non_separable(source, destination, color),
            Self::Luminosity => non_separable(source, destination, luminosity),
        }
    }
}

/// Immutable constant-color paint selected for one draw operation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Paint {
    color: Color,
    blend_mode: BlendMode,
}

impl Paint {
    /// Creates one source-over paint.
    pub const fn new(color: Color) -> Self {
        Self {
            color,
            blend_mode: BlendMode::SourceOver,
        }
    }

    /// Selects the source color.
    pub const fn with_color(mut self, color: Color) -> Self {
        self.color = color;
        self
    }

    /// Replaces the source alpha while retaining its RGB channels.
    pub const fn with_alpha(mut self, alpha: u8) -> Self {
        self.color = self.color.with_alpha(alpha);
        self
    }

    /// Multiplies the source alpha by one 8-bit opacity factor.
    pub fn with_opacity(mut self, opacity: u8) -> Self {
        self.color = self.color.with_opacity(opacity);
        self
    }

    /// Selects a compositing operation.
    pub const fn with_blend_mode(mut self, blend_mode: BlendMode) -> Self {
        self.blend_mode = blend_mode;
        self
    }

    /// Returns the straight source color.
    pub const fn color(self) -> Color {
        self.color
    }

    /// Returns the compositing operation.
    pub const fn blend_mode(self) -> BlendMode {
        self.blend_mode
    }
}

impl Default for Paint {
    fn default() -> Self {
        Self::new(Color::BLACK)
    }
}

impl Color {
    fn canonicalized(self) -> Self {
        if self.alpha == 0 {
            Self::TRANSPARENT
        } else {
            self
        }
    }
}

fn porter_duff(
    source: Color,
    destination: Color,
    source_factor: u32,
    destination_factor: u32,
) -> Color {
    let source = Premul::from(source);
    let destination = Premul::from(destination);
    Premul {
        red: add_sat(
            mul_255(source.red, source_factor),
            mul_255(destination.red, destination_factor),
        ),
        green: add_sat(
            mul_255(source.green, source_factor),
            mul_255(destination.green, destination_factor),
        ),
        blue: add_sat(
            mul_255(source.blue, source_factor),
            mul_255(destination.blue, destination_factor),
        ),
        alpha: add_sat(
            mul_255(source.alpha, source_factor),
            mul_255(destination.alpha, destination_factor),
        ),
    }
    .into()
}

fn plus(source: Color, destination: Color) -> Color {
    let source = Premul::from(source);
    let destination = Premul::from(destination);
    Premul {
        red: add_sat(source.red, destination.red),
        green: add_sat(source.green, destination.green),
        blue: add_sat(source.blue, destination.blue),
        alpha: add_sat(source.alpha, destination.alpha),
    }
    .into()
}

fn modulate(source: Color, destination: Color) -> Color {
    let source = Premul::from(source);
    let destination = Premul::from(destination);
    Premul {
        red: mul_255(source.red, destination.red),
        green: mul_255(source.green, destination.green),
        blue: mul_255(source.blue, destination.blue),
        alpha: mul_255(source.alpha, destination.alpha),
    }
    .into()
}

fn separable(source: Color, destination: Color, blend: impl Fn(u32, u32) -> u32) -> Color {
    let source_premul = Premul::from(source);
    let destination_premul = Premul::from(destination);
    let alpha = add_sat(
        source_premul.alpha,
        mul_255(destination_premul.alpha, 255 - source_premul.alpha),
    );
    let channel = |source: u8, destination: u8, source_channel: u32, destination_channel: u32| {
        let outside_source = mul_255(source_channel, 255 - destination_premul.alpha);
        let outside_destination = mul_255(destination_channel, 255 - source_premul.alpha);
        let overlap = mul_255(
            mul_255(
                blend(u32::from(source), u32::from(destination)),
                source_premul.alpha,
            ),
            destination_premul.alpha,
        );
        add_sat(add_sat(outside_source, outside_destination), overlap)
    };
    Premul {
        red: channel(
            source.red,
            destination.red,
            source_premul.red,
            destination_premul.red,
        ),
        green: channel(
            source.green,
            destination.green,
            source_premul.green,
            destination_premul.green,
        ),
        blue: channel(
            source.blue,
            destination.blue,
            source_premul.blue,
            destination_premul.blue,
        ),
        alpha,
    }
    .into()
}

fn non_separable(
    source: Color,
    destination: Color,
    blend: impl Fn([i32; 3], [i32; 3]) -> [i32; 3],
) -> Color {
    let source_premul = Premul::from(source);
    let destination_premul = Premul::from(destination);
    let alpha = add_sat(
        source_premul.alpha,
        mul_255(destination_premul.alpha, 255 - source_premul.alpha),
    );
    let blended = blend(
        [
            i32::from(source.red),
            i32::from(source.green),
            i32::from(source.blue),
        ],
        [
            i32::from(destination.red),
            i32::from(destination.green),
            i32::from(destination.blue),
        ],
    );
    let channel = |source: u32, destination: u32, blended: i32| {
        add_sat(
            add_sat(
                mul_255(source, 255 - destination_premul.alpha),
                mul_255(destination, 255 - source_premul.alpha),
            ),
            mul_255(
                mul_255(blended.clamp(0, 255) as u32, source_premul.alpha),
                destination_premul.alpha,
            ),
        )
    };
    Premul {
        red: channel(source_premul.red, destination_premul.red, blended[0]),
        green: channel(source_premul.green, destination_premul.green, blended[1]),
        blue: channel(source_premul.blue, destination_premul.blue, blended[2]),
        alpha,
    }
    .into()
}

fn multiply(source: u32, destination: u32) -> u32 {
    mul_255(source, destination)
}
fn screen(source: u32, destination: u32) -> u32 {
    source + destination - mul_255(source, destination)
}
fn overlay(source: u32, destination: u32) -> u32 {
    if destination <= 127 {
        mul_255(2 * source, destination)
    } else {
        255 - mul_255(2 * (255 - source), 255 - destination)
    }
}
fn color_dodge(source: u32, destination: u32) -> u32 {
    if source == 255 {
        255
    } else {
        (destination * 255 / (255 - source)).min(255)
    }
}
fn color_burn(source: u32, destination: u32) -> u32 {
    if source == 0 {
        0
    } else {
        255 - ((255 - destination) * 255 / source).min(255)
    }
}
fn soft_light(source: u32, destination: u32) -> u32 {
    if source <= 127 {
        destination - mul_255(mul_255(255 - 2 * source, destination), 255 - destination)
    } else {
        let dark = if destination <= 63 {
            ((16 * destination - 12 * 255) * destination + 4 * 255 * 255) * destination
                / (255 * 255)
        } else {
            integer_sqrt(destination * 255)
        };
        destination + mul_255(2 * source - 255, dark - destination)
    }
}
fn exclusion(source: u32, destination: u32) -> u32 {
    source + destination - 2 * mul_255(source, destination)
}

fn hue(source: [i32; 3], destination: [i32; 3]) -> [i32; 3] {
    set_lum(set_sat(source, sat(destination)), lum(destination))
}
fn saturation(source: [i32; 3], destination: [i32; 3]) -> [i32; 3] {
    set_lum(set_sat(destination, sat(source)), lum(destination))
}
fn color(source: [i32; 3], destination: [i32; 3]) -> [i32; 3] {
    set_lum(source, lum(destination))
}
fn luminosity(source: [i32; 3], destination: [i32; 3]) -> [i32; 3] {
    set_lum(destination, lum(source))
}

fn lum(color: [i32; 3]) -> i32 {
    (77 * color[0] + 150 * color[1] + 29 * color[2] + 128) / 256
}
fn sat(color: [i32; 3]) -> i32 {
    color.into_iter().max().unwrap_or(0) - color.into_iter().min().unwrap_or(0)
}
fn set_lum(mut color: [i32; 3], target: i32) -> [i32; 3] {
    let delta = target - lum(color);
    for channel in &mut color {
        *channel += delta;
    }
    clip_color(color)
}
fn clip_color(mut color: [i32; 3]) -> [i32; 3] {
    let luminance = lum(color);
    let minimum = color.into_iter().min().unwrap_or(0);
    let maximum = color.into_iter().max().unwrap_or(0);
    if minimum < 0 {
        for channel in &mut color {
            *channel = luminance + (*channel - luminance) * luminance / (luminance - minimum);
        }
    }
    if maximum > 255 {
        for channel in &mut color {
            *channel =
                luminance + (*channel - luminance) * (255 - luminance) / (maximum - luminance);
        }
    }
    color.map(|channel| channel.clamp(0, 255))
}
fn set_sat(color: [i32; 3], target: i32) -> [i32; 3] {
    let mut order = [0_usize, 1, 2];
    order.sort_by_key(|&index| color[index]);
    let mut result = [0; 3];
    let minimum = color[order[0]];
    let maximum = color[order[2]];
    if maximum > minimum {
        result[order[1]] = (color[order[1]] - minimum) * target / (maximum - minimum);
        result[order[2]] = target;
    }
    result
}

#[derive(Clone, Copy)]
struct Premul {
    red: u32,
    green: u32,
    blue: u32,
    alpha: u32,
}
impl From<Color> for Premul {
    fn from(color: Color) -> Self {
        Self {
            red: mul_255(u32::from(color.red), u32::from(color.alpha)),
            green: mul_255(u32::from(color.green), u32::from(color.alpha)),
            blue: mul_255(u32::from(color.blue), u32::from(color.alpha)),
            alpha: u32::from(color.alpha),
        }
    }
}
impl From<Premul> for Color {
    fn from(value: Premul) -> Self {
        if value.alpha == 0 {
            return Self::TRANSPARENT;
        }
        Self::rgba(
            to_u8(round_div(value.red * 255, value.alpha)),
            to_u8(round_div(value.green * 255, value.alpha)),
            to_u8(round_div(value.blue * 255, value.alpha)),
            to_u8(value.alpha),
        )
    }
}

fn mul_255(left: u32, right: u32) -> u32 {
    round_div(left * right, 255)
}
fn round_div(numerator: u32, denominator: u32) -> u32 {
    (numerator + denominator / 2) / denominator
}
fn add_sat(left: u32, right: u32) -> u32 {
    left.saturating_add(right).min(255)
}
fn to_u8(value: u32) -> u8 {
    u8::try_from(value.min(255)).unwrap_or(u8::MAX)
}
fn integer_sqrt(value: u32) -> u32 {
    let mut low = 0;
    let mut high = 256;
    while low < high {
        let middle = (low + high + 1) / 2;
        if middle * middle <= value {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    low
}

#[cfg(test)]
mod tests {
    use super::{BlendMode, Color, Paint};

    #[test]
    fn color_constructors_pack_channels_and_apply_opacity() {
        let color = Color::rgba(1, 2, 3, 128);
        assert_eq!(color.channels(), [1, 2, 3, 128]);
        assert_eq!(color.argb(), 0x8001_0203);
        assert_eq!(color.rgba_u32(), 0x0102_0380);
        assert_eq!(Color::from_argb(0x8001_0203), color);
        assert_eq!(Color::from_rgba_u32(0x0102_0380), color);
        assert_eq!(color.with_alpha(64).alpha(), 64);
        assert_eq!(color.with_opacity(128).alpha(), 64);
        assert!(Color::BLACK.is_opaque());
        assert!(Color::TRANSPARENT.is_transparent());
    }

    #[test]
    fn paint_builders_are_immutable_and_default_to_black_source_over() {
        let paint = Paint::default()
            .with_color(Color::RED)
            .with_alpha(64)
            .with_opacity(128)
            .with_blend_mode(BlendMode::Screen);
        assert_eq!(Paint::default(), Paint::new(Color::BLACK));
        assert_eq!(paint.color(), Color::rgba(255, 0, 0, 32));
        assert_eq!(paint.blend_mode(), BlendMode::Screen);
    }

    #[test]
    fn porter_duff_modes_cover_source_destination_and_alpha_edges() {
        let source = Color::rgba(255, 0, 0, 128);
        let destination = Color::rgba(0, 0, 255, 255);
        assert_eq!(
            source.composite(destination, BlendMode::SourceOver),
            Color::rgba(128, 0, 127, 255)
        );
        assert_eq!(
            source.composite(destination, BlendMode::DestinationOver),
            destination
        );
        assert_eq!(source.composite(destination, BlendMode::SourceIn), source);
        assert_eq!(
            source.composite(destination, BlendMode::DestinationOut),
            Color::rgba(0, 0, 255, 127)
        );
        assert_eq!(
            source.composite(Color::TRANSPARENT, BlendMode::SourceOver),
            source
        );
    }

    #[test]
    fn advanced_modes_are_defined_for_every_8_bit_input() {
        let source = Color::rgba(100, 200, 50, 192);
        let destination = Color::rgba(200, 100, 250, 128);
        let modes = [
            BlendMode::Clear,
            BlendMode::Source,
            BlendMode::Destination,
            BlendMode::SourceOver,
            BlendMode::DestinationOver,
            BlendMode::SourceIn,
            BlendMode::DestinationIn,
            BlendMode::SourceOut,
            BlendMode::DestinationOut,
            BlendMode::SourceAtop,
            BlendMode::DestinationAtop,
            BlendMode::Xor,
            BlendMode::Plus,
            BlendMode::Modulate,
            BlendMode::Multiply,
            BlendMode::Screen,
            BlendMode::Overlay,
            BlendMode::Darken,
            BlendMode::Lighten,
            BlendMode::ColorDodge,
            BlendMode::ColorBurn,
            BlendMode::HardLight,
            BlendMode::SoftLight,
            BlendMode::Difference,
            BlendMode::Exclusion,
            BlendMode::Hue,
            BlendMode::Saturation,
            BlendMode::Color,
            BlendMode::Luminosity,
        ];
        for mode in modes {
            let color = source.composite(destination, mode);
            assert!(color.alpha() <= u8::MAX, "{mode:?}");
        }
        assert_eq!(
            Color::rgb(100, 200, 50).composite(Color::rgb(200, 100, 250), BlendMode::Multiply),
            Color::rgb(78, 78, 49)
        );
        assert_eq!(
            Color::rgb(100, 200, 50).composite(Color::rgb(200, 100, 250), BlendMode::Screen),
            Color::rgb(222, 222, 251)
        );
    }
}
