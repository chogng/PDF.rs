use crate::{
    ContentOperatorSource, ContentVmError, ContentVmErrorCode, ContentVmLimit, ContentVmLimitKind,
};

const HARD_MAX_OPERATORS: u64 = 50_000_000;
const HARD_MAX_FUEL: u64 = 20_000_000_000;
const HARD_MAX_GRAPHICS_STATE_DEPTH: u32 = 65_536;
const HARD_MAX_COMPATIBILITY_DEPTH: u32 = 65_536;
const HARD_MAX_MARKED_CONTENT_DEPTH: u32 = 65_536;
const HARD_MAX_PROPERTY_USES: u64 = 50_000_000;
const HARD_MAX_RETAINED_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Unvalidated deterministic limits for one Content VM interpretation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContentVmLimitConfig {
    /// Maximum page-global operators admitted by the VM.
    pub max_operators: u64,
    /// Maximum deterministic VM work units.
    pub max_fuel: u64,
    /// Maximum saved graphics-state nesting depth.
    pub max_graphics_state_depth: u32,
    /// Maximum active compatibility-section nesting depth.
    pub max_compatibility_depth: u32,
    /// Maximum active marked-content nesting depth.
    pub max_marked_content_depth: u32,
    /// Maximum marked-content property references retained by the interpreted result.
    pub max_property_uses: u64,
    /// Maximum allocator-reported capacity retained by VM-owned state.
    pub max_retained_bytes: u64,
}

impl Default for ContentVmLimitConfig {
    fn default() -> Self {
        Self {
            max_operators: 4_000_000,
            max_fuel: 1_000_000_000,
            max_graphics_state_depth: 4_096,
            max_compatibility_depth: 4_096,
            max_marked_content_depth: 4_096,
            max_property_uses: 1_000_000,
            max_retained_bytes: 512 * 1024 * 1024,
        }
    }
}

/// Fully validated deterministic limits for one Content VM interpretation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContentVmLimits {
    max_operators: u64,
    max_fuel: u64,
    max_graphics_state_depth: u32,
    max_compatibility_depth: u32,
    max_marked_content_depth: u32,
    max_property_uses: u64,
    max_retained_bytes: u64,
}

impl ContentVmLimits {
    /// Validates every nonzero dimension against fixed implementation hard ceilings.
    pub fn validate(config: ContentVmLimitConfig) -> Result<Self, ContentVmError> {
        if config.max_operators == 0
            || config.max_operators > HARD_MAX_OPERATORS
            || config.max_fuel == 0
            || config.max_fuel > HARD_MAX_FUEL
            || config.max_graphics_state_depth == 0
            || config.max_graphics_state_depth > HARD_MAX_GRAPHICS_STATE_DEPTH
            || config.max_compatibility_depth == 0
            || config.max_compatibility_depth > HARD_MAX_COMPATIBILITY_DEPTH
            || config.max_marked_content_depth == 0
            || config.max_marked_content_depth > HARD_MAX_MARKED_CONTENT_DEPTH
            || config.max_property_uses == 0
            || config.max_property_uses > HARD_MAX_PROPERTY_USES
            || config.max_retained_bytes == 0
            || config.max_retained_bytes > HARD_MAX_RETAINED_BYTES
        {
            return Err(ContentVmError::new(ContentVmErrorCode::InvalidLimits, None));
        }
        Ok(Self {
            max_operators: config.max_operators,
            max_fuel: config.max_fuel,
            max_graphics_state_depth: config.max_graphics_state_depth,
            max_compatibility_depth: config.max_compatibility_depth,
            max_marked_content_depth: config.max_marked_content_depth,
            max_property_uses: config.max_property_uses,
            max_retained_bytes: config.max_retained_bytes,
        })
    }

    /// Returns the maximum page-global operator count.
    pub const fn max_operators(self) -> u64 {
        self.max_operators
    }

    /// Returns the maximum deterministic VM fuel.
    pub const fn max_fuel(self) -> u64 {
        self.max_fuel
    }

    /// Returns the maximum saved graphics-state depth.
    pub const fn max_graphics_state_depth(self) -> u32 {
        self.max_graphics_state_depth
    }

    /// Returns the maximum active compatibility-section depth.
    pub const fn max_compatibility_depth(self) -> u32 {
        self.max_compatibility_depth
    }

    /// Returns the maximum active marked-content depth.
    pub const fn max_marked_content_depth(self) -> u32 {
        self.max_marked_content_depth
    }

    /// Returns the maximum retained marked-content property-use count.
    pub const fn max_property_uses(self) -> u64 {
        self.max_property_uses
    }

    /// Returns the maximum allocator-reported VM retention.
    pub const fn max_retained_bytes(self) -> u64 {
        self.max_retained_bytes
    }

    /// Preflights one additional charge against an independent VM budget.
    ///
    /// `consumed` is the amount already committed and `attempted` is the additional charge. The
    /// returned resource error retains only numeric budget context and optional operator
    /// provenance.
    pub fn preflight(
        self,
        kind: ContentVmLimitKind,
        consumed: u64,
        attempted: u64,
        source: Option<ContentOperatorSource>,
    ) -> Result<(), ContentVmError> {
        let limit = match kind {
            ContentVmLimitKind::Operators => self.max_operators,
            ContentVmLimitKind::Fuel => self.max_fuel,
            ContentVmLimitKind::GraphicsStateDepth => u64::from(self.max_graphics_state_depth),
            ContentVmLimitKind::CompatibilityDepth => u64::from(self.max_compatibility_depth),
            ContentVmLimitKind::MarkedContentDepth => u64::from(self.max_marked_content_depth),
            ContentVmLimitKind::PropertyUses => self.max_property_uses,
            ContentVmLimitKind::RetainedBytes | ContentVmLimitKind::Allocation => {
                self.max_retained_bytes
            }
        };
        if consumed
            .checked_add(attempted)
            .is_none_or(|next| next > limit)
        {
            return Err(ContentVmError::resource(
                ContentVmLimit::new(kind, limit, consumed, attempted),
                source,
            ));
        }
        Ok(())
    }
}

impl Default for ContentVmLimits {
    fn default() -> Self {
        Self::validate(ContentVmLimitConfig::default())
            .expect("built-in Content VM limits satisfy hard ceilings")
    }
}
