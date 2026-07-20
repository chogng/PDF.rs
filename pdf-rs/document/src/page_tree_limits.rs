use crate::{DocumentError, DocumentErrorCode};

const HARD_MAX_NODES: u64 = 4_000_000;
const HARD_MAX_DEPTH: u64 = 1024;
const HARD_MAX_PAGES: u64 = 4_000_000;
const HARD_MAX_KIDS: u64 = 1024 * 1024;
const HARD_MAX_TOTAL_OBJECT_BYTES: u64 = 1024 * 1024 * 1024;
const HARD_MAX_RETAINED_TRAVERSAL_BYTES: u64 = 512 * 1024 * 1024;

/// Unvalidated deterministic limits for one strict page-tree traversal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PageTreeLimitConfig {
    /// Maximum page-tree node objects started by one traversal.
    pub max_nodes: u64,
    /// Maximum exact node references in one active root-to-node path.
    pub max_depth: u64,
    /// Maximum leaf page dictionaries accepted by one traversal.
    pub max_pages: u64,
    /// Maximum entries accepted from any one page-tree `/Kids` array.
    pub max_kids_per_node: u64,
    /// Maximum cumulative exact-read bytes across all child object jobs.
    pub max_total_object_read_bytes: u64,
    /// Maximum cumulative parser-window bytes across all child object jobs.
    pub max_total_object_parse_bytes: u64,
    /// Maximum allocator-reported capacity retained by traversal-owned containers.
    pub max_retained_traversal_bytes: u64,
}

impl Default for PageTreeLimitConfig {
    fn default() -> Self {
        Self {
            max_nodes: 100_000,
            max_depth: 64,
            max_pages: 25_000,
            max_kids_per_node: 8 * 1024,
            max_total_object_read_bytes: 64 * 1024 * 1024,
            max_total_object_parse_bytes: 64 * 1024 * 1024,
            max_retained_traversal_bytes: 16 * 1024 * 1024,
        }
    }
}

/// Validated deterministic limits for one strict page-tree traversal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PageTreeLimits {
    max_nodes: u64,
    max_depth: u64,
    max_pages: u64,
    max_kids_per_node: u64,
    max_total_object_read_bytes: u64,
    max_total_object_parse_bytes: u64,
    max_retained_traversal_bytes: u64,
    effective_work_items: u64,
    effective_seen_references: u64,
}

impl PageTreeLimits {
    /// Validates nonzero limits, fixed hard ceilings, and checked derived work bounds.
    pub fn validate(config: PageTreeLimitConfig) -> Result<Self, DocumentError> {
        let effective_work_items = config.max_nodes.checked_mul(2);

        if config.max_nodes == 0
            || config.max_nodes > HARD_MAX_NODES
            || config.max_depth == 0
            || config.max_depth > HARD_MAX_DEPTH
            || config.max_pages == 0
            || config.max_pages > HARD_MAX_PAGES
            || config.max_kids_per_node == 0
            || config.max_kids_per_node > HARD_MAX_KIDS
            || config.max_total_object_read_bytes == 0
            || config.max_total_object_read_bytes > HARD_MAX_TOTAL_OBJECT_BYTES
            || config.max_total_object_parse_bytes == 0
            || config.max_total_object_parse_bytes > HARD_MAX_TOTAL_OBJECT_BYTES
            || config.max_retained_traversal_bytes == 0
            || config.max_retained_traversal_bytes > HARD_MAX_RETAINED_TRAVERSAL_BYTES
            || effective_work_items.is_none()
        {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidLimits,
                None,
                None,
            ));
        }

        Ok(Self {
            max_nodes: config.max_nodes,
            max_depth: config.max_depth,
            max_pages: config.max_pages,
            max_kids_per_node: config.max_kids_per_node,
            max_total_object_read_bytes: config.max_total_object_read_bytes,
            max_total_object_parse_bytes: config.max_total_object_parse_bytes,
            max_retained_traversal_bytes: config.max_retained_traversal_bytes,
            effective_work_items: effective_work_items
                .expect("validated page-tree node limit has a checked work-item bound"),
            effective_seen_references: config.max_nodes,
        })
    }

    /// Returns the maximum page-tree node objects started.
    pub const fn max_nodes(self) -> u64 {
        self.max_nodes
    }

    /// Returns the maximum exact node references in one active path.
    pub const fn max_depth(self) -> u64 {
        self.max_depth
    }

    /// Returns the maximum leaf page dictionaries accepted.
    pub const fn max_pages(self) -> u64 {
        self.max_pages
    }

    /// Returns the maximum entries accepted from any one `/Kids` array.
    pub const fn max_kids_per_node(self) -> u64 {
        self.max_kids_per_node
    }

    /// Returns the cumulative exact-read ceiling across child object jobs.
    pub const fn max_total_object_read_bytes(self) -> u64 {
        self.max_total_object_read_bytes
    }

    /// Returns the cumulative parser-window ceiling across child object jobs.
    pub const fn max_total_object_parse_bytes(self) -> u64 {
        self.max_total_object_parse_bytes
    }

    /// Returns the allocator-reported retained traversal-capacity ceiling.
    pub const fn max_retained_traversal_bytes(self) -> u64 {
        self.max_retained_traversal_bytes
    }

    /// Returns the checked aggregate work-item bound derived from the node limit.
    pub const fn effective_work_items(self) -> u64 {
        self.effective_work_items
    }

    /// Returns the checked seen-reference bound derived from the node limit.
    pub const fn effective_seen_references(self) -> u64 {
        self.effective_seen_references
    }
}

impl Default for PageTreeLimits {
    fn default() -> Self {
        Self::validate(PageTreeLimitConfig::default())
            .expect("built-in page-tree limits satisfy hard ceilings")
    }
}
