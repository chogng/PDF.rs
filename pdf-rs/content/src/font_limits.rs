use crate::{
    ContentFontLimit, ContentFontLimitKind, ContentOperatorSource, ContentVmError,
    ContentVmErrorCode,
};

const HARD_MAX_FONT_USES: u64 = 50_000_000;
const HARD_MAX_UNIQUE_FONTS: u64 = 1_000_000;
const HARD_MAX_RESOURCE_RETAINED_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const HARD_MAX_GLYPHS: u64 = 100_000_000;
const HARD_MAX_OUTLINE_SEGMENTS: u64 = 500_000_000;
const HARD_MAX_GLYPH_RETAINED_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const HARD_MAX_TEXT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const HARD_MAX_TEXT_ADJUSTMENTS: u64 = 100_000_000;
const HARD_MAX_PLANNING_OPERATORS: u64 = 100_000_000;
const HARD_MAX_CACHE_PROBES: u64 = 100_000_000;
const HARD_MAX_PLAN_RETAINED_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const HARD_MAX_CACHE_RETAINED_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const HARD_MAX_ACQUISITION_POLLS: u64 = 50_000_000;

/// Unvalidated aggregate limits for embedded fonts and text used by one Content interpretation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContentFontLimitConfig {
    /// Maximum executed `Tf` font selections.
    pub max_font_uses: u64,
    /// Maximum distinct proof-bound embedded fonts.
    pub max_unique_fonts: u64,
    /// Maximum aggregate retained bytes across acquired proof-bound Font resources.
    pub max_resource_retained_bytes: u64,
    /// Maximum printable character codes expanded into positioned glyphs.
    pub max_glyphs: u64,
    /// Maximum aggregate outline segments copied into Scene glyph resources.
    pub max_outline_segments: u64,
    /// Maximum allocator-reported glyph/outlines live before one Scene handoff.
    pub max_glyph_retained_bytes: u64,
    /// Maximum decoded printable string bytes retained by the semantic plan.
    pub max_text_bytes: u64,
    /// Maximum numeric `TJ` adjustments retained by the semantic plan.
    pub max_text_adjustments: u64,
    /// Maximum operators inspected by the one text-planning pass.
    pub max_planning_operators: u64,
    /// Maximum exact-font-cache key comparisons.
    pub max_cache_probes: u64,
    /// Maximum allocator-reported text/operator/proof planning capacity.
    pub max_plan_retained_bytes: u64,
    /// Maximum allocator-reported exact-font-cache metadata capacity.
    pub max_cache_retained_bytes: u64,
    /// Maximum calls admitted into lower Font resource acquisition jobs.
    pub max_acquisition_polls: u64,
}

impl Default for ContentFontLimitConfig {
    fn default() -> Self {
        Self {
            max_font_uses: 1_000_000,
            max_unique_fonts: 65_536,
            max_resource_retained_bytes: 1024 * 1024 * 1024,
            max_glyphs: 4_000_000,
            max_outline_segments: 16_000_000,
            max_glyph_retained_bytes: 512 * 1024 * 1024,
            max_text_bytes: 256 * 1024 * 1024,
            max_text_adjustments: 4_000_000,
            max_planning_operators: 4_000_000,
            max_cache_probes: 4_000_000,
            max_plan_retained_bytes: 256 * 1024 * 1024,
            max_cache_retained_bytes: 256 * 1024 * 1024,
            max_acquisition_polls: 1_000_000,
        }
    }
}

/// Fully validated aggregate limits for embedded-font Content execution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContentFontLimits {
    config: ContentFontLimitConfig,
}

impl ContentFontLimits {
    /// Validates every positive dimension against a fixed implementation hard ceiling.
    pub fn validate(config: ContentFontLimitConfig) -> Result<Self, ContentVmError> {
        if config.max_font_uses == 0
            || config.max_font_uses > HARD_MAX_FONT_USES
            || config.max_unique_fonts == 0
            || config.max_unique_fonts > HARD_MAX_UNIQUE_FONTS
            || config.max_resource_retained_bytes == 0
            || config.max_resource_retained_bytes > HARD_MAX_RESOURCE_RETAINED_BYTES
            || config.max_glyphs == 0
            || config.max_glyphs > HARD_MAX_GLYPHS
            || config.max_outline_segments == 0
            || config.max_outline_segments > HARD_MAX_OUTLINE_SEGMENTS
            || config.max_glyph_retained_bytes == 0
            || config.max_glyph_retained_bytes > HARD_MAX_GLYPH_RETAINED_BYTES
            || config.max_text_bytes == 0
            || config.max_text_bytes > HARD_MAX_TEXT_BYTES
            || config.max_text_adjustments == 0
            || config.max_text_adjustments > HARD_MAX_TEXT_ADJUSTMENTS
            || config.max_planning_operators == 0
            || config.max_planning_operators > HARD_MAX_PLANNING_OPERATORS
            || config.max_cache_probes == 0
            || config.max_cache_probes > HARD_MAX_CACHE_PROBES
            || config.max_plan_retained_bytes == 0
            || config.max_plan_retained_bytes > HARD_MAX_PLAN_RETAINED_BYTES
            || config.max_cache_retained_bytes == 0
            || config.max_cache_retained_bytes > HARD_MAX_CACHE_RETAINED_BYTES
            || config.max_acquisition_polls == 0
            || config.max_acquisition_polls > HARD_MAX_ACQUISITION_POLLS
        {
            return Err(ContentVmError::new(ContentVmErrorCode::InvalidLimits, None));
        }
        Ok(Self { config })
    }

    /// Returns the executed `Tf` ceiling.
    pub const fn max_font_uses(self) -> u64 {
        self.config.max_font_uses
    }

    /// Returns the distinct embedded-font ceiling.
    pub const fn max_unique_fonts(self) -> u64 {
        self.config.max_unique_fonts
    }

    /// Returns the aggregate acquired Font resource retention ceiling.
    pub const fn max_resource_retained_bytes(self) -> u64 {
        self.config.max_resource_retained_bytes
    }

    /// Returns the positioned-glyph ceiling.
    pub const fn max_glyphs(self) -> u64 {
        self.config.max_glyphs
    }

    /// Returns the aggregate copied outline-segment ceiling.
    pub const fn max_outline_segments(self) -> u64 {
        self.config.max_outline_segments
    }

    /// Returns the allocator-reported live glyph candidate ceiling.
    pub const fn max_glyph_retained_bytes(self) -> u64 {
        self.config.max_glyph_retained_bytes
    }

    /// Returns the decoded printable text-byte ceiling.
    pub const fn max_text_bytes(self) -> u64 {
        self.config.max_text_bytes
    }

    /// Returns the retained `TJ` numeric-adjustment ceiling.
    pub const fn max_text_adjustments(self) -> u64 {
        self.config.max_text_adjustments
    }

    /// Returns the one-pass text-planning operator ceiling.
    pub const fn max_planning_operators(self) -> u64 {
        self.config.max_planning_operators
    }

    /// Returns the exact-cache comparison ceiling.
    pub const fn max_cache_probes(self) -> u64 {
        self.config.max_cache_probes
    }

    /// Returns the semantic-plan retained-capacity ceiling.
    pub const fn max_plan_retained_bytes(self) -> u64 {
        self.config.max_plan_retained_bytes
    }

    /// Returns the exact-font-cache retained-capacity ceiling.
    pub const fn max_cache_retained_bytes(self) -> u64 {
        self.config.max_cache_retained_bytes
    }

    /// Returns the lower acquisition-poll ceiling.
    pub const fn max_acquisition_polls(self) -> u64 {
        self.config.max_acquisition_polls
    }

    pub(crate) fn preflight(
        self,
        kind: ContentFontLimitKind,
        consumed: u64,
        attempted: u64,
        source: Option<ContentOperatorSource>,
    ) -> Result<(), ContentVmError> {
        let limit =
            match kind {
                ContentFontLimitKind::FontUses => self.max_font_uses(),
                ContentFontLimitKind::UniqueFonts => self.max_unique_fonts(),
                ContentFontLimitKind::ResourceRetainedBytes => self.max_resource_retained_bytes(),
                ContentFontLimitKind::Glyphs => self.max_glyphs(),
                ContentFontLimitKind::OutlineSegments => self.max_outline_segments(),
                ContentFontLimitKind::GlyphRetainedBytes
                | ContentFontLimitKind::GlyphAllocation => self.max_glyph_retained_bytes(),
                ContentFontLimitKind::TextBytes => self.max_text_bytes(),
                ContentFontLimitKind::TextAdjustments => self.max_text_adjustments(),
                ContentFontLimitKind::PlanningOperators => self.max_planning_operators(),
                ContentFontLimitKind::CacheProbes => self.max_cache_probes(),
                ContentFontLimitKind::PlanRetainedBytes | ContentFontLimitKind::PlanAllocation => {
                    self.max_plan_retained_bytes()
                }
                ContentFontLimitKind::CacheRetainedBytes
                | ContentFontLimitKind::CacheAllocation => self.max_cache_retained_bytes(),
                ContentFontLimitKind::AcquisitionPolls => self.max_acquisition_polls(),
            };
        if consumed
            .checked_add(attempted)
            .is_none_or(|next| next > limit)
        {
            return Err(ContentVmError::font_resource(
                ContentFontLimit::new(kind, limit, consumed, attempted),
                source,
            ));
        }
        Ok(())
    }
}

impl Default for ContentFontLimits {
    fn default() -> Self {
        Self::validate(ContentFontLimitConfig::default())
            .expect("built-in Content font limits satisfy hard ceilings")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_limits_validate_and_report_independent_dimensions() {
        let minimum = ContentFontLimits::validate(ContentFontLimitConfig {
            max_font_uses: 1,
            max_unique_fonts: 1,
            max_resource_retained_bytes: 1,
            max_glyphs: 1,
            max_outline_segments: 1,
            max_glyph_retained_bytes: 1,
            max_text_bytes: 1,
            max_text_adjustments: 1,
            max_planning_operators: 1,
            max_cache_probes: 1,
            max_plan_retained_bytes: 1,
            max_cache_retained_bytes: 1,
            max_acquisition_polls: 1,
        })
        .unwrap();
        assert_eq!(minimum.max_glyphs(), 1);

        let error = ContentFontLimits::default()
            .preflight(
                ContentFontLimitKind::Glyphs,
                ContentFontLimits::default().max_glyphs(),
                1,
                None,
            )
            .unwrap_err();
        assert_eq!(error.code(), ContentVmErrorCode::ResourceLimit);
        assert_eq!(
            error.font_limit().unwrap().kind(),
            ContentFontLimitKind::Glyphs
        );
    }
}
