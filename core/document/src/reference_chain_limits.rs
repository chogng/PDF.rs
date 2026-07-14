use std::mem;

use pdf_rs_syntax::ObjectRef;

use crate::{DocumentError, DocumentErrorCode};

const HARD_MAX_OBJECTS: u64 = 256;
const HARD_MAX_REFERENCE_EDGES: u64 = 256;
const HARD_MAX_DEPTH: u64 = 256;
const HARD_MAX_TOTAL_OBJECT_BYTES: u64 = 256 * 1024 * 1024;
const HARD_MAX_RETAINED_PATH_BYTES: u64 = 64 * 1024;

/// Unvalidated limits for one bounded top-level direct-reference chain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReferenceChainLimitConfig {
    /// Maximum proof-preserving object jobs started by one chain job.
    pub max_objects: u64,
    /// Maximum top-level indirect-reference edges followed by one chain job.
    pub max_reference_edges: u64,
    /// Maximum distinct exact references in the active chain.
    pub max_depth: u64,
    /// Maximum cumulative exact-read bytes across all child object jobs.
    pub max_total_object_read_bytes: u64,
    /// Maximum cumulative parser-window bytes across all child object jobs.
    pub max_total_object_parse_bytes: u64,
    /// Maximum allocator-reported capacity retained by the reference path.
    pub max_retained_path_bytes: u64,
}

impl Default for ReferenceChainLimitConfig {
    fn default() -> Self {
        Self {
            max_objects: 64,
            max_reference_edges: 64,
            max_depth: 64,
            max_total_object_read_bytes: 64 * 1024 * 1024,
            max_total_object_parse_bytes: 64 * 1024 * 1024,
            max_retained_path_bytes: 4 * 1024,
        }
    }
}

/// Validated deterministic limits for one bounded top-level direct-reference chain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReferenceChainLimits {
    max_objects: u64,
    max_reference_edges: u64,
    max_depth: u64,
    max_total_object_read_bytes: u64,
    max_total_object_parse_bytes: u64,
    max_retained_path_bytes: u64,
    effective_path_references: u64,
}

impl ReferenceChainLimits {
    /// Validates nonzero limits, fixed hard ceilings, and retained-path capacity.
    pub fn validate(config: ReferenceChainLimitConfig) -> Result<Self, DocumentError> {
        let edge_references = config.max_reference_edges.checked_add(1);
        let effective_path_references =
            edge_references.map(|edges| config.max_objects.min(edges).min(config.max_depth));
        let object_ref_bytes = u64::try_from(mem::size_of::<ObjectRef>()).ok();
        let required_path_bytes = effective_path_references
            .zip(object_ref_bytes)
            .and_then(|(references, bytes)| references.checked_mul(bytes));

        if config.max_objects == 0
            || config.max_objects > HARD_MAX_OBJECTS
            || config.max_reference_edges == 0
            || config.max_reference_edges > HARD_MAX_REFERENCE_EDGES
            || config.max_depth == 0
            || config.max_depth > HARD_MAX_DEPTH
            || config.max_total_object_read_bytes == 0
            || config.max_total_object_read_bytes > HARD_MAX_TOTAL_OBJECT_BYTES
            || config.max_total_object_parse_bytes == 0
            || config.max_total_object_parse_bytes > HARD_MAX_TOTAL_OBJECT_BYTES
            || config.max_retained_path_bytes == 0
            || config.max_retained_path_bytes > HARD_MAX_RETAINED_PATH_BYTES
            || required_path_bytes.is_none_or(|required| config.max_retained_path_bytes < required)
        {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidLimits,
                None,
                None,
            ));
        }

        Ok(Self {
            max_objects: config.max_objects,
            max_reference_edges: config.max_reference_edges,
            max_depth: config.max_depth,
            max_total_object_read_bytes: config.max_total_object_read_bytes,
            max_total_object_parse_bytes: config.max_total_object_parse_bytes,
            max_retained_path_bytes: config.max_retained_path_bytes,
            effective_path_references: effective_path_references
                .expect("validated reference-chain limits have a checked effective path"),
        })
    }

    /// Returns the maximum child object jobs started by one chain job.
    pub const fn max_objects(self) -> u64 {
        self.max_objects
    }

    /// Returns the maximum top-level indirect-reference edges followed.
    pub const fn max_reference_edges(self) -> u64 {
        self.max_reference_edges
    }

    /// Returns the maximum distinct exact references in the active chain.
    pub const fn max_depth(self) -> u64 {
        self.max_depth
    }

    /// Returns the cumulative exact-read ceiling across child object jobs.
    pub const fn max_total_object_read_bytes(self) -> u64 {
        self.max_total_object_read_bytes
    }

    /// Returns the cumulative parser-window ceiling across child object jobs.
    pub const fn max_total_object_parse_bytes(self) -> u64 {
        self.max_total_object_parse_bytes
    }

    /// Returns the allocator-reported retained-path capacity ceiling.
    pub const fn max_retained_path_bytes(self) -> u64 {
        self.max_retained_path_bytes
    }

    pub(crate) const fn effective_path_references(self) -> u64 {
        self.effective_path_references
    }
}

impl Default for ReferenceChainLimits {
    fn default() -> Self {
        Self::validate(ReferenceChainLimitConfig::default())
            .expect("built-in reference-chain limits satisfy hard ceilings")
    }
}
