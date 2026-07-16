use crate::{FontError, FontErrorCode};

const HARD_MAX_INPUT_BYTES: u64 = 64 * 1024 * 1024;
const HARD_MAX_TABLES: u16 = 256;
const HARD_MAX_GLYPHS: u32 = u16::MAX as u32;
const HARD_MAX_CMAP_SEGMENTS: u32 = u16::MAX as u32 / 2;
const HARD_MAX_GLYPH_DATA_BYTES: u64 = 64 * 1024 * 1024;
const HARD_MAX_GLYPH_BYTES: u64 = 16 * 1024 * 1024;
const HARD_MAX_GLYPH_CONTOURS: u32 = 65_535;
const HARD_MAX_TOTAL_CONTOURS: u64 = 4 * 1024 * 1024;
const HARD_MAX_GLYPH_POINTS: u32 = 1024 * 1024;
const HARD_MAX_TOTAL_POINTS: u64 = 16 * 1024 * 1024;
const HARD_MAX_COMPONENTS: u64 = 4 * 1024 * 1024;
pub(crate) const HARD_MAX_COMPONENT_DEPTH: u16 = 64;
const HARD_MAX_PATH_SEGMENTS: u64 = 64 * 1024 * 1024;
const HARD_MAX_RETAINED_BYTES: u64 = 1024 * 1024 * 1024;
const HARD_MAX_FUEL: u64 = 8 * 1024 * 1024 * 1024;
const HARD_MAX_CANCELLATION_INTERVAL_FUEL: u64 = 1024 * 1024;

/// Unvalidated deterministic TrueType parsing limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FontLimitConfig {
    /// Maximum bytes in the complete sfnt program.
    pub max_input_bytes: u64,
    /// Maximum sfnt table-directory records.
    pub max_tables: u16,
    /// Maximum glyphs declared by `maxp`.
    pub max_glyphs: u32,
    /// Maximum segments in the selected format 4 character map.
    pub max_cmap_segments: u32,
    /// Maximum bytes addressed by `glyf`/`loca`.
    pub max_glyph_data_bytes: u64,
    /// Maximum bytes in one glyph description.
    pub max_glyph_bytes: u64,
    /// Maximum contours in one simple glyph.
    pub max_glyph_contours: u32,
    /// Maximum source contours across all simple glyphs.
    pub max_total_contours: u64,
    /// Maximum points in one simple glyph.
    pub max_glyph_points: u32,
    /// Maximum source points across all simple glyphs.
    pub max_total_points: u64,
    /// Maximum direct compound-component records across the font.
    pub max_components: u64,
    /// Maximum recursive compound-glyph expansion depth.
    pub max_component_depth: u16,
    /// Maximum project-owned outline segments after compound expansion.
    pub max_path_segments: u64,
    /// Maximum allocator-visible retained bytes at peak and after publication.
    pub max_retained_bytes: u64,
    /// Maximum deterministic parser work units.
    pub max_fuel: u64,
    /// Most fuel units allowed between cancellation probes.
    pub cancellation_check_interval_fuel: u64,
}

impl Default for FontLimitConfig {
    fn default() -> Self {
        Self {
            max_input_bytes: 16 * 1024 * 1024,
            max_tables: 64,
            max_glyphs: 16_384,
            max_cmap_segments: 4_096,
            max_glyph_data_bytes: 16 * 1024 * 1024,
            max_glyph_bytes: 2 * 1024 * 1024,
            max_glyph_contours: 16_384,
            max_total_contours: 1024 * 1024,
            max_glyph_points: 262_144,
            max_total_points: 4 * 1024 * 1024,
            max_components: 1024 * 1024,
            max_component_depth: 32,
            max_path_segments: 8 * 1024 * 1024,
            max_retained_bytes: 256 * 1024 * 1024,
            max_fuel: 512 * 1024 * 1024,
            cancellation_check_interval_fuel: 256,
        }
    }
}

/// Validated TrueType parsing limits beneath fixed implementation ceilings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FontLimits {
    pub(crate) max_input_bytes: u64,
    pub(crate) max_tables: u16,
    pub(crate) max_glyphs: u32,
    pub(crate) max_cmap_segments: u32,
    pub(crate) max_glyph_data_bytes: u64,
    pub(crate) max_glyph_bytes: u64,
    pub(crate) max_glyph_contours: u32,
    pub(crate) max_total_contours: u64,
    pub(crate) max_glyph_points: u32,
    pub(crate) max_total_points: u64,
    pub(crate) max_components: u64,
    pub(crate) max_component_depth: u16,
    pub(crate) max_path_segments: u64,
    pub(crate) max_retained_bytes: u64,
    pub(crate) max_fuel: u64,
    pub(crate) cancellation_check_interval_fuel: u64,
}

impl FontLimits {
    /// Validates a complete deterministic font budget profile.
    pub fn validate(config: FontLimitConfig) -> Result<Self, FontError> {
        if config.max_input_bytes == 0
            || config.max_input_bytes > HARD_MAX_INPUT_BYTES
            || config.max_tables == 0
            || config.max_tables > HARD_MAX_TABLES
            || config.max_glyphs == 0
            || config.max_glyphs > HARD_MAX_GLYPHS
            || config.max_cmap_segments == 0
            || config.max_cmap_segments > HARD_MAX_CMAP_SEGMENTS
            || config.max_glyph_data_bytes == 0
            || config.max_glyph_data_bytes > HARD_MAX_GLYPH_DATA_BYTES
            || config.max_glyph_data_bytes > config.max_input_bytes
            || config.max_glyph_bytes == 0
            || config.max_glyph_bytes > HARD_MAX_GLYPH_BYTES
            || config.max_glyph_bytes > config.max_glyph_data_bytes
            || config.max_glyph_contours == 0
            || config.max_glyph_contours > HARD_MAX_GLYPH_CONTOURS
            || config.max_total_contours == 0
            || config.max_total_contours > HARD_MAX_TOTAL_CONTOURS
            || u64::from(config.max_glyph_contours) > config.max_total_contours
            || config.max_glyph_points == 0
            || config.max_glyph_points > HARD_MAX_GLYPH_POINTS
            || config.max_total_points == 0
            || config.max_total_points > HARD_MAX_TOTAL_POINTS
            || u64::from(config.max_glyph_points) > config.max_total_points
            || config.max_components == 0
            || config.max_components > HARD_MAX_COMPONENTS
            || config.max_component_depth == 0
            || config.max_component_depth > HARD_MAX_COMPONENT_DEPTH
            || config.max_path_segments == 0
            || config.max_path_segments > HARD_MAX_PATH_SEGMENTS
            || config.max_retained_bytes == 0
            || config.max_retained_bytes > HARD_MAX_RETAINED_BYTES
            || config.max_fuel == 0
            || config.max_fuel > HARD_MAX_FUEL
            || config.cancellation_check_interval_fuel == 0
            || config.cancellation_check_interval_fuel > HARD_MAX_CANCELLATION_INTERVAL_FUEL
            || config.cancellation_check_interval_fuel > config.max_fuel
        {
            return Err(FontError::for_code(FontErrorCode::InvalidLimits, None));
        }
        Ok(Self {
            max_input_bytes: config.max_input_bytes,
            max_tables: config.max_tables,
            max_glyphs: config.max_glyphs,
            max_cmap_segments: config.max_cmap_segments,
            max_glyph_data_bytes: config.max_glyph_data_bytes,
            max_glyph_bytes: config.max_glyph_bytes,
            max_glyph_contours: config.max_glyph_contours,
            max_total_contours: config.max_total_contours,
            max_glyph_points: config.max_glyph_points,
            max_total_points: config.max_total_points,
            max_components: config.max_components,
            max_component_depth: config.max_component_depth,
            max_path_segments: config.max_path_segments,
            max_retained_bytes: config.max_retained_bytes,
            max_fuel: config.max_fuel,
            cancellation_check_interval_fuel: config.cancellation_check_interval_fuel,
        })
    }

    /// Returns the maximum complete input bytes.
    pub const fn max_input_bytes(self) -> u64 {
        self.max_input_bytes
    }
    /// Returns the maximum table-directory records.
    pub const fn max_tables(self) -> u16 {
        self.max_tables
    }
    /// Returns the maximum glyph count.
    pub const fn max_glyphs(self) -> u32 {
        self.max_glyphs
    }
    /// Returns the maximum selected `cmap` segment count.
    pub const fn max_cmap_segments(self) -> u32 {
        self.max_cmap_segments
    }
    /// Returns the maximum total glyph-data bytes.
    pub const fn max_glyph_data_bytes(self) -> u64 {
        self.max_glyph_data_bytes
    }
    /// Returns the maximum bytes in one glyph description.
    pub const fn max_glyph_bytes(self) -> u64 {
        self.max_glyph_bytes
    }
    /// Returns the maximum contours in one simple glyph.
    pub const fn max_glyph_contours(self) -> u32 {
        self.max_glyph_contours
    }
    /// Returns the maximum total source contours.
    pub const fn max_total_contours(self) -> u64 {
        self.max_total_contours
    }
    /// Returns the maximum points in one simple glyph.
    pub const fn max_glyph_points(self) -> u32 {
        self.max_glyph_points
    }
    /// Returns the maximum total source points.
    pub const fn max_total_points(self) -> u64 {
        self.max_total_points
    }
    /// Returns the maximum direct compound component records.
    pub const fn max_components(self) -> u64 {
        self.max_components
    }
    /// Returns the maximum compound expansion depth.
    pub const fn max_component_depth(self) -> u16 {
        self.max_component_depth
    }
    /// Returns the maximum expanded outline segments.
    pub const fn max_path_segments(self) -> u64 {
        self.max_path_segments
    }
    /// Returns the maximum allocator-visible retained bytes.
    pub const fn max_retained_bytes(self) -> u64 {
        self.max_retained_bytes
    }
    /// Returns the maximum deterministic work fuel.
    pub const fn max_fuel(self) -> u64 {
        self.max_fuel
    }
    /// Returns the maximum fuel units between cancellation probes.
    pub const fn cancellation_check_interval_fuel(self) -> u64 {
        self.cancellation_check_interval_fuel
    }
}

impl Default for FontLimits {
    fn default() -> Self {
        Self::validate(FontLimitConfig::default())
            .expect("built-in font limits satisfy fixed implementation ceilings")
    }
}
