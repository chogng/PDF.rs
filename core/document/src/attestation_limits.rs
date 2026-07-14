use crate::{DocumentError, DocumentErrorCode};

const HARD_MAX_SOURCE_BYTES: u64 = 1024 * 1024 * 1024;
const HARD_MAX_OBJECTS: u64 = 4_000_000;
const HARD_MAX_SCAN_CHUNK_BYTES: u64 = 1024 * 1024;
const HARD_MAX_TRIVIA_BYTES: u64 = 1024 * 1024 * 1024;
const HARD_MAX_COMMENT_BYTES: u64 = 1024 * 1024;
const HARD_MAX_TOTAL_OBJECT_WORK_BYTES: u64 = 1024 * 1024 * 1024;
const HARD_MAX_RETAINED_EVIDENCE_BYTES: u64 = 512 * 1024 * 1024;

/// Unvalidated deterministic limits for one strict base-revision attestation job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RevisionAttestationLimitConfig {
    /// Maximum immutable source length accepted by the attestation profile.
    pub max_source_bytes: u64,
    /// Maximum in-use objects framed before an attested index can be published.
    pub max_objects: u64,
    /// Maximum exact prefix or gap range requested in one scan step.
    pub scan_chunk_bytes: u64,
    /// Maximum cumulative exact prefix and gap bytes requested, including the header request.
    pub max_trivia_bytes: u64,
    /// Maximum bytes in one top-level comment, including its leading percent byte.
    pub max_comment_bytes: u64,
    /// Maximum cumulative exact bytes requested by all child object jobs.
    pub max_total_object_read_bytes: u64,
    /// Maximum cumulative parser-window bytes charged by all child object jobs.
    pub max_total_object_parse_bytes: u64,
    /// Maximum conservatively accounted retained object-attestation evidence capacity.
    pub max_retained_evidence_bytes: u64,
}

impl Default for RevisionAttestationLimitConfig {
    fn default() -> Self {
        Self {
            max_source_bytes: 256 * 1024 * 1024,
            max_objects: 25_000,
            scan_chunk_bytes: 64 * 1024,
            max_trivia_bytes: 64 * 1024 * 1024,
            max_comment_bytes: 64 * 1024,
            max_total_object_read_bytes: 64 * 1024 * 1024,
            max_total_object_parse_bytes: 64 * 1024 * 1024,
            max_retained_evidence_bytes: 8 * 1024 * 1024,
        }
    }
}

/// Validated strict base-revision attestation limits beneath fixed implementation ceilings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RevisionAttestationLimits {
    pub(crate) max_source_bytes: u64,
    pub(crate) max_objects: u64,
    pub(crate) scan_chunk_bytes: u64,
    pub(crate) max_trivia_bytes: u64,
    pub(crate) max_comment_bytes: u64,
    pub(crate) max_total_object_read_bytes: u64,
    pub(crate) max_total_object_parse_bytes: u64,
    pub(crate) max_retained_evidence_bytes: u64,
}

impl RevisionAttestationLimits {
    /// Validates one complete revision-attestation limit profile.
    pub fn validate(config: RevisionAttestationLimitConfig) -> Result<Self, DocumentError> {
        if config.max_source_bytes == 0
            || config.max_source_bytes > HARD_MAX_SOURCE_BYTES
            || config.max_objects == 0
            || config.max_objects > HARD_MAX_OBJECTS
            || config.scan_chunk_bytes < 9
            || config.scan_chunk_bytes > HARD_MAX_SCAN_CHUNK_BYTES
            || config.max_trivia_bytes < 9
            || config.max_trivia_bytes > HARD_MAX_TRIVIA_BYTES
            || config.scan_chunk_bytes > config.max_trivia_bytes
            || config.max_comment_bytes == 0
            || config.max_comment_bytes > HARD_MAX_COMMENT_BYTES
            || config.max_comment_bytes > config.max_trivia_bytes
            || config.max_total_object_read_bytes == 0
            || config.max_total_object_read_bytes > HARD_MAX_TOTAL_OBJECT_WORK_BYTES
            || config.max_total_object_parse_bytes == 0
            || config.max_total_object_parse_bytes > HARD_MAX_TOTAL_OBJECT_WORK_BYTES
            || config.max_retained_evidence_bytes == 0
            || config.max_retained_evidence_bytes > HARD_MAX_RETAINED_EVIDENCE_BYTES
        {
            return Err(DocumentError::for_code(
                DocumentErrorCode::InvalidLimits,
                None,
                None,
            ));
        }
        Ok(Self {
            max_source_bytes: config.max_source_bytes,
            max_objects: config.max_objects,
            scan_chunk_bytes: config.scan_chunk_bytes,
            max_trivia_bytes: config.max_trivia_bytes,
            max_comment_bytes: config.max_comment_bytes,
            max_total_object_read_bytes: config.max_total_object_read_bytes,
            max_total_object_parse_bytes: config.max_total_object_parse_bytes,
            max_retained_evidence_bytes: config.max_retained_evidence_bytes,
        })
    }

    /// Returns the maximum accepted immutable source length.
    pub const fn max_source_bytes(self) -> u64 {
        self.max_source_bytes
    }

    /// Returns the maximum number of objects attested in physical order.
    pub const fn max_objects(self) -> u64 {
        self.max_objects
    }

    /// Returns the maximum exact range requested by one trivia scan step.
    pub const fn scan_chunk_bytes(self) -> u64 {
        self.scan_chunk_bytes
    }

    /// Returns the cumulative prefix and gap read ceiling, including the header request.
    pub const fn max_trivia_bytes(self) -> u64 {
        self.max_trivia_bytes
    }

    /// Returns the maximum length of one top-level comment.
    pub const fn max_comment_bytes(self) -> u64 {
        self.max_comment_bytes
    }

    /// Returns the aggregate child-object exact-read ceiling.
    pub const fn max_total_object_read_bytes(self) -> u64 {
        self.max_total_object_read_bytes
    }

    /// Returns the aggregate child-object parse-window ceiling.
    pub const fn max_total_object_parse_bytes(self) -> u64 {
        self.max_total_object_parse_bytes
    }

    /// Returns the conservatively accounted retained-evidence capacity ceiling.
    pub const fn max_retained_evidence_bytes(self) -> u64 {
        self.max_retained_evidence_bytes
    }
}

impl Default for RevisionAttestationLimits {
    fn default() -> Self {
        Self::validate(RevisionAttestationLimitConfig::default())
            .expect("built-in revision-attestation limits satisfy hard ceilings")
    }
}
