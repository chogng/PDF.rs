use crate::{
    DESKTOP_FRAME_HEADER_BYTES, MAX_MESSAGE_BYTES, MAX_TRANSFER_SLOTS, ProtocolError,
    ProtocolErrorCode,
};

const HARD_MAX_FRAME_BYTES: u64 = 512 * 1024 * 1024;
const HARD_MAX_PAYLOAD_BYTES: u32 = 256 * 1024 * 1024;
const HARD_MAX_TRANSFER_SLOTS: u16 = 4_096;
const HARD_MAX_SURFACE_DIMENSION: u32 = 131_072;
const HARD_MAX_SURFACE_STRIDE_BYTES: u64 = 1024 * 1024 * 1024;
const HARD_MAX_SURFACE_BYTES: u64 = 16 * 1024 * 1024 * 1024;

/// Caller-selected protocol validation ceilings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProtocolLimitConfig {
    /// Maximum complete desktop frame bytes, including the fixed header.
    pub max_frame_bytes: u64,
    /// Maximum payload bytes accepted from any message.
    pub max_payload_bytes: u32,
    /// Maximum out-of-band transfer slots accepted on one message.
    pub max_transfer_slots: u16,
    /// Maximum Surface width or height.
    pub max_surface_dimension: u32,
    /// Maximum declared Surface row stride.
    pub max_surface_stride_bytes: u64,
    /// Maximum bytes addressable by one Surface.
    pub max_surface_bytes: u64,
}

impl Default for ProtocolLimitConfig {
    fn default() -> Self {
        let header = u64::try_from(DESKTOP_FRAME_HEADER_BYTES)
            .expect("the fixed protocol header length fits u64");
        Self {
            max_frame_bytes: header + u64::from(MAX_MESSAGE_BYTES),
            max_payload_bytes: MAX_MESSAGE_BYTES,
            max_transfer_slots: MAX_TRANSFER_SLOTS,
            max_surface_dimension: 32_768,
            max_surface_stride_bytes: 256 * 1024 * 1024,
            max_surface_bytes: 4 * 1024 * 1024 * 1024,
        }
    }
}

/// Validated hard-bounded protocol limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProtocolLimits {
    max_frame_bytes: u64,
    max_payload_bytes: u32,
    max_transfer_slots: u16,
    max_surface_dimension: u32,
    max_surface_stride_bytes: u64,
    max_surface_bytes: u64,
}

impl ProtocolLimits {
    /// Validates caller-selected ceilings without allocating.
    pub fn new(config: ProtocolLimitConfig) -> Result<Self, ProtocolError> {
        let header = u64::try_from(DESKTOP_FRAME_HEADER_BYTES)
            .map_err(|_| ProtocolError::for_code(ProtocolErrorCode::InvalidLimits))?;
        let largest_frame = header
            .checked_add(u64::from(config.max_payload_bytes))
            .ok_or_else(|| ProtocolError::for_code(ProtocolErrorCode::InvalidLimits))?;
        if config.max_frame_bytes < header
            || config.max_frame_bytes < largest_frame
            || config.max_frame_bytes > HARD_MAX_FRAME_BYTES
            || config.max_payload_bytes == 0
            || config.max_payload_bytes > HARD_MAX_PAYLOAD_BYTES
            || config.max_transfer_slots == 0
            || config.max_transfer_slots > HARD_MAX_TRANSFER_SLOTS
            || config.max_surface_dimension == 0
            || config.max_surface_dimension > HARD_MAX_SURFACE_DIMENSION
            || config.max_surface_stride_bytes == 0
            || config.max_surface_stride_bytes > HARD_MAX_SURFACE_STRIDE_BYTES
            || config.max_surface_bytes == 0
            || config.max_surface_bytes > HARD_MAX_SURFACE_BYTES
        {
            return Err(ProtocolError::for_code(ProtocolErrorCode::InvalidLimits));
        }
        Ok(Self {
            max_frame_bytes: config.max_frame_bytes,
            max_payload_bytes: config.max_payload_bytes,
            max_transfer_slots: config.max_transfer_slots,
            max_surface_dimension: config.max_surface_dimension,
            max_surface_stride_bytes: config.max_surface_stride_bytes,
            max_surface_bytes: config.max_surface_bytes,
        })
    }

    /// Returns the maximum complete desktop frame size.
    pub const fn max_frame_bytes(self) -> u64 {
        self.max_frame_bytes
    }

    /// Returns the maximum payload size accepted from any message.
    pub const fn max_payload_bytes(self) -> u32 {
        self.max_payload_bytes
    }

    /// Returns the maximum transfer slots accepted on one message.
    pub const fn max_transfer_slots(self) -> u16 {
        self.max_transfer_slots
    }

    /// Returns the maximum Surface width or height.
    pub const fn max_surface_dimension(self) -> u32 {
        self.max_surface_dimension
    }

    /// Returns the maximum Surface stride.
    pub const fn max_surface_stride_bytes(self) -> u64 {
        self.max_surface_stride_bytes
    }

    /// Returns the maximum bytes addressable by one Surface.
    pub const fn max_surface_bytes(self) -> u64 {
        self.max_surface_bytes
    }
}

impl Default for ProtocolLimits {
    fn default() -> Self {
        Self::new(ProtocolLimitConfig::default())
            .expect("the crate-owned protocol defaults obey their hard ceilings")
    }
}
