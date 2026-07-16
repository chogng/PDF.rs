use crate::{ContentError, ContentErrorCode};

const HARD_MAX_STREAMS: u32 = 1_000_000;
const HARD_MAX_DECODED_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const HARD_MAX_TOKENS: u64 = 100_000_000;
const HARD_MAX_TOKEN_BYTES: u64 = 256 * 1024 * 1024;
const HARD_MAX_OPERANDS_PER_OPERATOR: u32 = 1_000_000;
const HARD_MAX_NESTING_DEPTH: u16 = 256;
const HARD_MAX_OPERATORS: u64 = 50_000_000;
const HARD_MAX_FUEL: u64 = 20_000_000_000;
const HARD_MAX_RETAINED_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Unvalidated limits for one ordered decoded-content scan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContentLimitConfig {
    /// Maximum decoded streams in the page sequence.
    pub max_streams: u32,
    /// Maximum aggregate decoded bytes across all streams.
    pub max_total_decoded_bytes: u64,
    /// Maximum lexical tokens, including container delimiters and operators.
    pub max_tokens: u64,
    /// Maximum raw bytes in one lexical token.
    pub max_token_bytes: u64,
    /// Maximum top-level operands accumulated before one operator.
    pub max_operands_per_operator: u32,
    /// Maximum nested array/dictionary depth.
    pub max_nesting_depth: u16,
    /// Maximum operators retained in the published program.
    pub max_operators: u64,
    /// Maximum deterministic scanner work units.
    pub max_fuel: u64,
    /// Maximum allocator-reported bytes retained by owned scanner values.
    pub max_retained_bytes: u64,
}

impl Default for ContentLimitConfig {
    fn default() -> Self {
        Self {
            max_streams: 16_384,
            max_total_decoded_bytes: 256 * 1024 * 1024,
            max_tokens: 8_000_000,
            max_token_bytes: 16 * 1024 * 1024,
            max_operands_per_operator: 65_536,
            max_nesting_depth: 128,
            max_operators: 4_000_000,
            max_fuel: 1_000_000_000,
            max_retained_bytes: 512 * 1024 * 1024,
        }
    }
}

/// Fully validated limits for one ordered decoded-content scan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContentLimits {
    max_streams: u32,
    max_total_decoded_bytes: u64,
    max_tokens: u64,
    max_token_bytes: u64,
    max_operands_per_operator: u32,
    max_nesting_depth: u16,
    max_operators: u64,
    max_fuel: u64,
    max_retained_bytes: u64,
}

impl ContentLimits {
    /// Validates every nonzero limit against implementation hard ceilings.
    pub fn validate(config: ContentLimitConfig) -> Result<Self, ContentError> {
        if config.max_streams == 0
            || config.max_streams > HARD_MAX_STREAMS
            || config.max_total_decoded_bytes == 0
            || config.max_total_decoded_bytes > HARD_MAX_DECODED_BYTES
            || config.max_tokens == 0
            || config.max_tokens > HARD_MAX_TOKENS
            || config.max_token_bytes == 0
            || config.max_token_bytes > HARD_MAX_TOKEN_BYTES
            || config.max_operands_per_operator == 0
            || config.max_operands_per_operator > HARD_MAX_OPERANDS_PER_OPERATOR
            || config.max_nesting_depth == 0
            || config.max_nesting_depth > HARD_MAX_NESTING_DEPTH
            || config.max_operators == 0
            || config.max_operators > HARD_MAX_OPERATORS
            || config.max_fuel == 0
            || config.max_fuel > HARD_MAX_FUEL
            || config.max_retained_bytes == 0
            || config.max_retained_bytes > HARD_MAX_RETAINED_BYTES
        {
            return Err(ContentError::for_code(
                ContentErrorCode::InvalidLimits,
                None,
            ));
        }
        Ok(Self {
            max_streams: config.max_streams,
            max_total_decoded_bytes: config.max_total_decoded_bytes,
            max_tokens: config.max_tokens,
            max_token_bytes: config.max_token_bytes,
            max_operands_per_operator: config.max_operands_per_operator,
            max_nesting_depth: config.max_nesting_depth,
            max_operators: config.max_operators,
            max_fuel: config.max_fuel,
            max_retained_bytes: config.max_retained_bytes,
        })
    }

    /// Returns the maximum decoded stream count.
    pub const fn max_streams(self) -> u32 {
        self.max_streams
    }

    /// Returns the maximum aggregate decoded byte count.
    pub const fn max_total_decoded_bytes(self) -> u64 {
        self.max_total_decoded_bytes
    }

    /// Returns the maximum lexical token count.
    pub const fn max_tokens(self) -> u64 {
        self.max_tokens
    }

    /// Returns the maximum raw bytes in one token.
    pub const fn max_token_bytes(self) -> u64 {
        self.max_token_bytes
    }

    /// Returns the maximum top-level operands preceding one operator.
    pub const fn max_operands_per_operator(self) -> u32 {
        self.max_operands_per_operator
    }

    /// Returns the maximum array/dictionary nesting depth.
    pub const fn max_nesting_depth(self) -> u16 {
        self.max_nesting_depth
    }

    /// Returns the maximum published operator count.
    pub const fn max_operators(self) -> u64 {
        self.max_operators
    }

    /// Returns the maximum deterministic work-unit count.
    pub const fn max_fuel(self) -> u64 {
        self.max_fuel
    }

    /// Returns the maximum allocator-reported retained bytes.
    pub const fn max_retained_bytes(self) -> u64 {
        self.max_retained_bytes
    }
}

impl Default for ContentLimits {
    fn default() -> Self {
        Self::validate(ContentLimitConfig::default())
            .expect("built-in content limits satisfy hard ceilings")
    }
}
