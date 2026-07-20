/// One straight-alpha sRGBA8 color.
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
    pub const BLACK: Self = Self::rgba(0, 0, 0, u8::MAX);

    /// Opaque white.
    pub const WHITE: Self = Self::rgba(u8::MAX, u8::MAX, u8::MAX, u8::MAX);

    /// Creates a straight-alpha sRGBA8 color.
    pub const fn rgba(red: u8, green: u8, blue: u8, alpha: u8) -> Self {
        Self {
            red,
            green,
            blue,
            alpha,
        }
    }

    /// Returns channels in top-level RGBA order.
    pub const fn channels(self) -> [u8; 4] {
        [self.red, self.green, self.blue, self.alpha]
    }
}

/// Registered compositing operation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BlendMode {
    /// Standard straight-alpha source-over compositing.
    SourceOver,
}

/// Immutable paint selected for one draw operation.
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

    /// Selects a registered blend mode.
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
