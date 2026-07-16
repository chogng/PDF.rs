/// Deterministic Content VM work, nesting, and ownership accounting.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContentVmStats {
    operators: u64,
    fuel: u64,
    max_graphics_state_depth: u32,
    max_compatibility_depth: u32,
    max_marked_content_depth: u32,
    property_uses: u64,
    retained_bytes: u64,
}

impl ContentVmStats {
    pub(crate) const fn new(
        operators: u64,
        fuel: u64,
        max_graphics_state_depth: u32,
        max_compatibility_depth: u32,
        max_marked_content_depth: u32,
        property_uses: u64,
        retained_bytes: u64,
    ) -> Self {
        Self {
            operators,
            fuel,
            max_graphics_state_depth,
            max_compatibility_depth,
            max_marked_content_depth,
            property_uses,
            retained_bytes,
        }
    }

    /// Returns the admitted page-global operator count.
    pub const fn operators(self) -> u64 {
        self.operators
    }

    /// Returns deterministic VM work units.
    pub const fn fuel(self) -> u64 {
        self.fuel
    }

    /// Returns the deepest saved graphics-state nesting reached.
    pub const fn max_graphics_state_depth(self) -> u32 {
        self.max_graphics_state_depth
    }

    /// Returns the deepest compatibility-section nesting reached.
    pub const fn max_compatibility_depth(self) -> u32 {
        self.max_compatibility_depth
    }

    /// Returns the deepest marked-content nesting reached.
    pub const fn max_marked_content_depth(self) -> u32 {
        self.max_marked_content_depth
    }

    /// Returns the marked-content property-reference count.
    pub const fn property_uses(self) -> u64 {
        self.property_uses
    }

    /// Returns allocator-reported capacity retained by VM-owned state.
    pub const fn retained_bytes(self) -> u64 {
        self.retained_bytes
    }
}

impl Default for ContentVmStats {
    fn default() -> Self {
        Self::new(0, 0, 0, 0, 0, 0, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::ContentVmStats;

    #[test]
    fn nonzero_stats_report_each_independent_dimension() {
        let stats = ContentVmStats::new(11, 29, 3, 5, 7, 13, 1_024);
        assert_eq!(stats.operators(), 11);
        assert_eq!(stats.fuel(), 29);
        assert_eq!(stats.max_graphics_state_depth(), 3);
        assert_eq!(stats.max_compatibility_depth(), 5);
        assert_eq!(stats.max_marked_content_depth(), 7);
        assert_eq!(stats.property_uses(), 13);
        assert_eq!(stats.retained_bytes(), 1_024);
    }
}
