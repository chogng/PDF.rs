use crate::{SyntaxError, SyntaxErrorCode};

const HARD_MAX_INPUT_BYTES: u64 = 64 * 1024 * 1024;
const HARD_MAX_TOKEN_BYTES: u64 = 16 * 1024 * 1024;
const HARD_MAX_COMMENT_BYTES: u64 = 1024 * 1024;
const HARD_MAX_NAME_BYTES: u64 = 1024 * 1024;
const HARD_MAX_STRING_BYTES: u64 = 32 * 1024 * 1024;
const HARD_MAX_OWNED_BYTES: u64 = 64 * 1024 * 1024;
const HARD_MAX_TOKENS: u64 = 4_000_000;
const HARD_MAX_CONTAINER_ENTRIES: u64 = 1_000_000;
const HARD_MAX_CONTAINER_BYTES: u64 = 256 * 1024 * 1024;
const HARD_MAX_CONTAINER_DEPTH: u16 = 512;

/// Unvalidated deterministic PDF syntax limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SyntaxLimitConfig {
    /// Maximum bytes in one contiguous parser input window.
    pub max_input_bytes: u64,
    /// Maximum source bytes in one token.
    pub max_token_bytes: u64,
    /// Maximum source bytes in one comment.
    pub max_comment_bytes: u64,
    /// Maximum decoded bytes in one name.
    pub max_name_bytes: u64,
    /// Maximum source bytes scanned for one string.
    pub max_string_source_bytes: u64,
    /// Maximum decoded bytes retained for one string.
    pub max_string_decoded_bytes: u64,
    /// Maximum cumulative decoded scalar bytes retained by one parser attempt.
    pub max_owned_bytes: u64,
    /// Maximum tokens consumed by one parser attempt.
    pub max_total_tokens: u64,
    /// Maximum cumulative array items and dictionary entries.
    pub max_container_entries: u64,
    /// Maximum allocator-reported array and dictionary vector capacity bytes.
    pub max_container_bytes: u64,
    /// Maximum nested array and dictionary depth.
    pub max_container_depth: u16,
}

impl Default for SyntaxLimitConfig {
    fn default() -> Self {
        Self {
            max_input_bytes: 8 * 1024 * 1024,
            max_token_bytes: 1024 * 1024,
            max_comment_bytes: 64 * 1024,
            max_name_bytes: 64 * 1024,
            max_string_source_bytes: 4 * 1024 * 1024,
            max_string_decoded_bytes: 4 * 1024 * 1024,
            max_owned_bytes: 8 * 1024 * 1024,
            max_total_tokens: 250_000,
            max_container_entries: 100_000,
            max_container_bytes: 64 * 1024 * 1024,
            max_container_depth: 128,
        }
    }
}

/// Validated syntax limits beneath fixed implementation ceilings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SyntaxLimits {
    pub(crate) max_input_bytes: u64,
    pub(crate) max_token_bytes: u64,
    pub(crate) max_comment_bytes: u64,
    pub(crate) max_name_bytes: u64,
    pub(crate) max_string_source_bytes: u64,
    pub(crate) max_string_decoded_bytes: u64,
    pub(crate) max_owned_bytes: u64,
    pub(crate) max_total_tokens: u64,
    pub(crate) max_container_entries: u64,
    pub(crate) max_container_bytes: u64,
    pub(crate) max_container_depth: u16,
}

impl SyntaxLimits {
    /// Validates a complete syntax budget profile.
    pub fn validate(config: SyntaxLimitConfig) -> Result<Self, SyntaxError> {
        if config.max_input_bytes == 0
            || config.max_input_bytes > HARD_MAX_INPUT_BYTES
            || config.max_token_bytes == 0
            || config.max_token_bytes > HARD_MAX_TOKEN_BYTES
            || config.max_token_bytes > config.max_input_bytes
            || config.max_comment_bytes == 0
            || config.max_comment_bytes > HARD_MAX_COMMENT_BYTES
            || config.max_comment_bytes > config.max_token_bytes
            || config.max_name_bytes == 0
            || config.max_name_bytes > HARD_MAX_NAME_BYTES
            || config.max_name_bytes > config.max_token_bytes
            || config.max_string_source_bytes == 0
            || config.max_string_source_bytes > HARD_MAX_STRING_BYTES
            || config.max_string_source_bytes > config.max_input_bytes
            || config.max_string_decoded_bytes == 0
            || config.max_string_decoded_bytes > HARD_MAX_STRING_BYTES
            || config.max_owned_bytes == 0
            || config.max_owned_bytes > HARD_MAX_OWNED_BYTES
            || config.max_owned_bytes < config.max_name_bytes
            || config.max_owned_bytes < config.max_string_decoded_bytes
            || config.max_total_tokens == 0
            || config.max_total_tokens > HARD_MAX_TOKENS
            || config.max_container_entries == 0
            || config.max_container_entries > HARD_MAX_CONTAINER_ENTRIES
            || config.max_container_bytes == 0
            || config.max_container_bytes > HARD_MAX_CONTAINER_BYTES
            || config.max_container_depth == 0
            || config.max_container_depth > HARD_MAX_CONTAINER_DEPTH
        {
            return Err(SyntaxError::for_code(SyntaxErrorCode::InvalidLimits, None));
        }
        Ok(Self {
            max_input_bytes: config.max_input_bytes,
            max_token_bytes: config.max_token_bytes,
            max_comment_bytes: config.max_comment_bytes,
            max_name_bytes: config.max_name_bytes,
            max_string_source_bytes: config.max_string_source_bytes,
            max_string_decoded_bytes: config.max_string_decoded_bytes,
            max_owned_bytes: config.max_owned_bytes,
            max_total_tokens: config.max_total_tokens,
            max_container_entries: config.max_container_entries,
            max_container_bytes: config.max_container_bytes,
            max_container_depth: config.max_container_depth,
        })
    }

    /// Returns the maximum contiguous input bytes.
    pub const fn max_input_bytes(self) -> u64 {
        self.max_input_bytes
    }

    /// Returns the maximum token source bytes.
    pub const fn max_token_bytes(self) -> u64 {
        self.max_token_bytes
    }

    /// Returns the maximum comment source bytes.
    pub const fn max_comment_bytes(self) -> u64 {
        self.max_comment_bytes
    }

    /// Returns the maximum decoded name bytes.
    pub const fn max_name_bytes(self) -> u64 {
        self.max_name_bytes
    }

    /// Returns the maximum string source bytes.
    pub const fn max_string_source_bytes(self) -> u64 {
        self.max_string_source_bytes
    }

    /// Returns the maximum decoded string bytes.
    pub const fn max_string_decoded_bytes(self) -> u64 {
        self.max_string_decoded_bytes
    }

    /// Returns the maximum cumulative owned scalar bytes.
    pub const fn max_owned_bytes(self) -> u64 {
        self.max_owned_bytes
    }

    /// Returns the maximum token count.
    pub const fn max_total_tokens(self) -> u64 {
        self.max_total_tokens
    }

    /// Returns the maximum cumulative container entries.
    pub const fn max_container_entries(self) -> u64 {
        self.max_container_entries
    }

    /// Returns the maximum allocator-reported container capacity bytes.
    pub const fn max_container_bytes(self) -> u64 {
        self.max_container_bytes
    }

    /// Returns the maximum container depth.
    pub const fn max_container_depth(self) -> u16 {
        self.max_container_depth
    }
}

impl Default for SyntaxLimits {
    fn default() -> Self {
        Self::validate(SyntaxLimitConfig::default())
            .expect("built-in syntax limits satisfy hard ceilings")
    }
}
