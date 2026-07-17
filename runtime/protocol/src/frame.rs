use std::fmt;

use crate::{
    EnvelopeHeader, MAX_MESSAGE_BYTES, MAX_TRANSFER_SLOTS, PROTOCOL_MAJOR, PROTOCOL_MINOR,
    ProtocolError, ProtocolErrorCode, ProtocolLimits,
};

/// Fixed desktop binary envelope header size.
///
/// The generated logical [`EnvelopeHeader`] is encoded as four little-endian `u16` values, one
/// little-endian `u32` payload length, and one little-endian `u64` direction-local sequence.
pub const DESKTOP_FRAME_HEADER_BYTES: usize = 20;

const MAJOR_OFFSET: usize = 0;
const MINOR_OFFSET: usize = 2;
const MESSAGE_TYPE_OFFSET: usize = 4;
const FLAGS_OFFSET: usize = 6;
const PAYLOAD_LEN_OFFSET: usize = 8;
const SEQUENCE_OFFSET: usize = 12;

/// Generated-descriptor policy needed to validate one desktop frame.
///
/// This is validation state, not a second wire descriptor registry. Callers obtain it from the
/// generated message descriptor through [`crate::ProtocolValidator`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameMessagePolicy {
    message_type: u16,
    allowed_flags: u16,
    max_payload_bytes: u32,
    min_transfer_slots: u16,
    max_transfer_slots: u16,
}

impl FrameMessagePolicy {
    pub(crate) fn new(
        message_type: u16,
        allowed_flags: u16,
        max_payload_bytes: u32,
        min_transfer_slots: u16,
        max_transfer_slots: u16,
    ) -> Result<Self, ProtocolError> {
        if max_payload_bytes == 0
            || max_payload_bytes > MAX_MESSAGE_BYTES
            || min_transfer_slots > max_transfer_slots
            || max_transfer_slots > MAX_TRANSFER_SLOTS
        {
            return Err(ProtocolError::for_code(
                ProtocolErrorCode::InvalidGeneratedDescriptor,
            ));
        }
        Ok(Self {
            message_type,
            allowed_flags,
            max_payload_bytes,
            min_transfer_slots,
            max_transfer_slots,
        })
    }

    /// Returns the generated message identifier.
    pub const fn message_type(self) -> u16 {
        self.message_type
    }

    /// Returns the complete bit mask accepted in the frame header.
    pub const fn allowed_flags(self) -> u16 {
        self.allowed_flags
    }

    /// Returns the generated message-specific payload ceiling.
    pub const fn max_payload_bytes(self) -> u32 {
        self.max_payload_bytes
    }

    /// Returns the minimum required out-of-band transfer count.
    pub const fn min_transfer_slots(self) -> u16 {
        self.min_transfer_slots
    }

    /// Returns the maximum accepted out-of-band transfer count.
    pub const fn max_transfer_slots(self) -> u16 {
        self.max_transfer_slots
    }
}

/// Accepted receive-direction sequence state.
///
/// One tracker belongs to exactly one sending direction. Sequence gaps are valid because sequence
/// detects duplicate or regressing frames rather than imposing cross-direction total order.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SequenceTracker {
    last_accepted: Option<u64>,
}

impl SequenceTracker {
    /// Creates an empty receive-direction tracker.
    pub const fn new() -> Self {
        Self {
            last_accepted: None,
        }
    }

    /// Returns the most recently accepted sequence.
    pub const fn last_accepted(self) -> Option<u64> {
        self.last_accepted
    }

    fn validate(&self, candidate: u64) -> Result<(), ProtocolError> {
        if candidate == 0
            || self
                .last_accepted
                .is_some_and(|last_accepted| candidate <= last_accepted)
        {
            return Err(ProtocolError::for_code(
                ProtocolErrorCode::NonMonotonicSequence,
            ));
        }
        Ok(())
    }

    fn commit(&mut self, accepted: u64) {
        self.last_accepted = Some(accepted);
    }
}

/// Fully frame-validated borrowed desktop payload.
///
/// The payload remains borrowed from the transport buffer and is never copied by frame validation.
/// Higher-level generated payload decoding must still validate correlation and message fields
/// before dispatching a command or event to runtime code.
#[derive(Clone, Eq, PartialEq)]
pub struct ValidatedDesktopFrame<'frame> {
    header: EnvelopeHeader,
    policy: FrameMessagePolicy,
    transfer_slots: u16,
    payload: &'frame [u8],
}

impl<'frame> ValidatedDesktopFrame<'frame> {
    /// Returns the validated generated envelope header.
    pub const fn header(&self) -> &EnvelopeHeader {
        &self.header
    }

    /// Returns the generated validation policy selected by message identifier.
    pub const fn policy(&self) -> FrameMessagePolicy {
        self.policy
    }

    /// Returns the observed out-of-band transfer count.
    pub const fn transfer_slots(&self) -> u16 {
        self.transfer_slots
    }

    /// Borrows the length-checked payload for generated typed decoding.
    pub const fn payload(&self) -> &'frame [u8] {
        self.payload
    }
}

impl fmt::Debug for ValidatedDesktopFrame<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ValidatedDesktopFrame")
            .field("header", &self.header)
            .field("policy", &self.policy)
            .field("transfer_slots", &self.transfer_slots)
            .field("payload_len", &self.payload.len())
            .field("payload", &"[REDACTED]")
            .finish()
    }
}

/// Stateless desktop frame validator with fixed negotiated version and resource limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DesktopFrameDecoder {
    major: u16,
    minor: u16,
    limits: ProtocolLimits,
}

impl DesktopFrameDecoder {
    /// Creates a decoder for the crate's current exact schema version.
    pub const fn current(limits: ProtocolLimits) -> Self {
        Self {
            major: PROTOCOL_MAJOR,
            minor: PROTOCOL_MINOR,
            limits,
        }
    }

    /// Creates a decoder for a version selected by a successful handshake.
    pub const fn negotiated(major: u16, minor: u16, limits: ProtocolLimits) -> Self {
        Self {
            major,
            minor,
            limits,
        }
    }

    /// Returns the exact negotiated protocol major.
    pub const fn major(self) -> u16 {
        self.major
    }

    /// Returns the exact negotiated protocol minor.
    pub const fn minor(self) -> u16 {
        self.minor
    }

    /// Returns the validated global limits.
    pub const fn limits(self) -> ProtocolLimits {
        self.limits
    }

    /// Validates one complete desktop frame before borrowing its payload.
    ///
    /// `policy` must be selected from the generated descriptor with the identifier present in the
    /// fixed header. `transfer_slots` is the actual out-of-band handle/transfer table length
    /// delivered with this frame. The direction-local sequence advances only after every check
    /// succeeds, so malformed high-sequence input cannot desynchronize a connection.
    pub fn decode<'frame>(
        &self,
        frame: &'frame [u8],
        transfer_slots: usize,
        policy: FrameMessagePolicy,
        sequence: &mut SequenceTracker,
    ) -> Result<ValidatedDesktopFrame<'frame>, ProtocolError> {
        if frame.len() < DESKTOP_FRAME_HEADER_BYTES {
            return Err(ProtocolError::for_code(ProtocolErrorCode::TruncatedHeader));
        }
        let frame_len = u64::try_from(frame.len())
            .map_err(|_| ProtocolError::for_code(ProtocolErrorCode::FrameTooLarge))?;
        if frame_len > self.limits.max_frame_bytes() {
            return Err(ProtocolError::for_code(ProtocolErrorCode::FrameTooLarge));
        }

        let header = decode_header(frame)?;
        if header.major != self.major {
            return Err(ProtocolError::for_code(ProtocolErrorCode::UnsupportedMajor));
        }
        if header.minor != self.minor {
            return Err(ProtocolError::for_code(ProtocolErrorCode::UnsupportedMinor));
        }
        if header.message_type != policy.message_type {
            return Err(ProtocolError::for_code(ProtocolErrorCode::UnknownMessage));
        }
        if header.flags & !policy.allowed_flags != 0 {
            return Err(ProtocolError::for_code(ProtocolErrorCode::InvalidFlags));
        }
        if header.payload_len > self.limits.max_payload_bytes() {
            return Err(ProtocolError::for_code(ProtocolErrorCode::PayloadTooLarge));
        }
        if header.payload_len > policy.max_payload_bytes {
            return Err(ProtocolError::for_code(
                ProtocolErrorCode::MessagePayloadTooLarge,
            ));
        }

        let declared_frame_len = u64::try_from(DESKTOP_FRAME_HEADER_BYTES)
            .map_err(|_| ProtocolError::for_code(ProtocolErrorCode::NumericOverflow))?
            .checked_add(u64::from(header.payload_len))
            .ok_or_else(|| ProtocolError::for_code(ProtocolErrorCode::NumericOverflow))?;
        if declared_frame_len != frame_len {
            return Err(ProtocolError::for_code(
                ProtocolErrorCode::FrameLengthMismatch,
            ));
        }
        if transfer_slots > usize::from(self.limits.max_transfer_slots())
            || transfer_slots < usize::from(policy.min_transfer_slots)
            || transfer_slots > usize::from(policy.max_transfer_slots)
        {
            return Err(ProtocolError::for_code(
                ProtocolErrorCode::InvalidTransferCount,
            ));
        }
        let transfer_slots = u16::try_from(transfer_slots)
            .map_err(|_| ProtocolError::for_code(ProtocolErrorCode::InvalidTransferCount))?;
        sequence.validate(header.sequence)?;

        let payload = frame
            .get(DESKTOP_FRAME_HEADER_BYTES..)
            .ok_or_else(|| ProtocolError::for_code(ProtocolErrorCode::FrameLengthMismatch))?;
        sequence.commit(header.sequence);
        Ok(ValidatedDesktopFrame {
            header,
            policy,
            transfer_slots,
            payload,
        })
    }
}

fn decode_header(frame: &[u8]) -> Result<EnvelopeHeader, ProtocolError> {
    Ok(EnvelopeHeader {
        major: decode_u16(frame, MAJOR_OFFSET)?,
        minor: decode_u16(frame, MINOR_OFFSET)?,
        message_type: decode_u16(frame, MESSAGE_TYPE_OFFSET)?,
        flags: decode_u16(frame, FLAGS_OFFSET)?,
        payload_len: decode_u32(frame, PAYLOAD_LEN_OFFSET)?,
        sequence: decode_u64(frame, SEQUENCE_OFFSET)?,
    })
}

fn decode_u16(frame: &[u8], offset: usize) -> Result<u16, ProtocolError> {
    let bytes: [u8; 2] = frame
        .get(offset..offset + 2)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| ProtocolError::for_code(ProtocolErrorCode::TruncatedHeader))?;
    Ok(u16::from_le_bytes(bytes))
}

fn decode_u32(frame: &[u8], offset: usize) -> Result<u32, ProtocolError> {
    let bytes: [u8; 4] = frame
        .get(offset..offset + 4)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| ProtocolError::for_code(ProtocolErrorCode::TruncatedHeader))?;
    Ok(u32::from_le_bytes(bytes))
}

fn decode_u64(frame: &[u8], offset: usize) -> Result<u64, ProtocolError> {
    let bytes: [u8; 8] = frame
        .get(offset..offset + 8)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or_else(|| ProtocolError::for_code(ProtocolErrorCode::TruncatedHeader))?;
    Ok(u64::from_le_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::{
        DESKTOP_FRAME_HEADER_BYTES, DesktopFrameDecoder, FrameMessagePolicy, SequenceTracker,
    };
    use crate::{
        PROTOCOL_MAJOR, PROTOCOL_MINOR, ProtocolErrorCode, ProtocolLimitConfig, ProtocolLimits,
    };

    fn limits(max_payload_bytes: u32, max_transfer_slots: u16) -> ProtocolLimits {
        ProtocolLimits::new(ProtocolLimitConfig {
            max_frame_bytes: u64::try_from(DESKTOP_FRAME_HEADER_BYTES).unwrap()
                + u64::from(max_payload_bytes),
            max_payload_bytes,
            max_transfer_slots,
            max_surface_dimension: 4_096,
            max_surface_stride_bytes: 64 * 1024 * 1024,
            max_surface_bytes: 256 * 1024 * 1024,
        })
        .unwrap()
    }

    fn policy(
        message_type: u16,
        flags: u16,
        max_payload: u32,
        min_slots: u16,
        max_slots: u16,
    ) -> FrameMessagePolicy {
        FrameMessagePolicy::new(message_type, flags, max_payload, min_slots, max_slots).unwrap()
    }

    fn frame(
        major: u16,
        minor: u16,
        message_type: u16,
        flags: u16,
        declared_payload: u32,
        sequence: u64,
        payload: &[u8],
    ) -> Vec<u8> {
        let mut output = Vec::with_capacity(DESKTOP_FRAME_HEADER_BYTES + payload.len());
        output.extend_from_slice(&major.to_le_bytes());
        output.extend_from_slice(&minor.to_le_bytes());
        output.extend_from_slice(&message_type.to_le_bytes());
        output.extend_from_slice(&flags.to_le_bytes());
        output.extend_from_slice(&declared_payload.to_le_bytes());
        output.extend_from_slice(&sequence.to_le_bytes());
        output.extend_from_slice(payload);
        output
    }

    #[test]
    fn exact_frame_is_borrowed_and_commits_sequence_last() {
        let limits = limits(32, 4);
        let decoder = DesktopFrameDecoder::current(limits);
        let policy = policy(7, 0b0011, 16, 1, 2);
        let bytes = frame(PROTOCOL_MAJOR, PROTOCOL_MINOR, 7, 1, 3, 41, b"abc");
        let mut sequence = SequenceTracker::new();

        let accepted = decoder.decode(&bytes, 1, policy, &mut sequence).unwrap();

        assert_eq!(accepted.header().payload_len, 3);
        assert_eq!(accepted.payload(), b"abc");
        assert_eq!(accepted.transfer_slots(), 1);
        assert_eq!(sequence.last_accepted(), Some(41));
    }

    #[test]
    fn framing_version_message_flags_and_lengths_fail_before_sequence_commit() {
        let limits = limits(8, 2);
        let decoder = DesktopFrameDecoder::current(limits);
        let policy = policy(7, 1, 4, 0, 1);
        let valid = frame(PROTOCOL_MAJOR, PROTOCOL_MINOR, 7, 0, 1, 9, b"x");
        let cases = [
            (
                valid[..DESKTOP_FRAME_HEADER_BYTES - 1].to_vec(),
                0,
                ProtocolErrorCode::TruncatedHeader,
            ),
            (
                {
                    let mut oversized = valid.clone();
                    oversized.extend_from_slice(&[0; 8]);
                    oversized
                },
                0,
                ProtocolErrorCode::FrameTooLarge,
            ),
            (
                frame(PROTOCOL_MAJOR + 1, PROTOCOL_MINOR, 7, 0, 1, 9, b"x"),
                0,
                ProtocolErrorCode::UnsupportedMajor,
            ),
            (
                frame(PROTOCOL_MAJOR, PROTOCOL_MINOR + 1, 7, 0, 1, 9, b"x"),
                0,
                ProtocolErrorCode::UnsupportedMinor,
            ),
            (
                frame(PROTOCOL_MAJOR, PROTOCOL_MINOR, 8, 0, 1, 9, b"x"),
                0,
                ProtocolErrorCode::UnknownMessage,
            ),
            (
                frame(PROTOCOL_MAJOR, PROTOCOL_MINOR, 7, 2, 1, 9, b"x"),
                0,
                ProtocolErrorCode::InvalidFlags,
            ),
            (
                frame(PROTOCOL_MAJOR, PROTOCOL_MINOR, 7, 0, 9, 9, b"x"),
                0,
                ProtocolErrorCode::PayloadTooLarge,
            ),
            (
                frame(PROTOCOL_MAJOR, PROTOCOL_MINOR, 7, 0, 5, 9, b"x"),
                0,
                ProtocolErrorCode::MessagePayloadTooLarge,
            ),
            (
                frame(PROTOCOL_MAJOR, PROTOCOL_MINOR, 7, 0, 2, 9, b"x"),
                0,
                ProtocolErrorCode::FrameLengthMismatch,
            ),
            (valid.clone(), 2, ProtocolErrorCode::InvalidTransferCount),
            (
                valid.clone(),
                usize::MAX,
                ProtocolErrorCode::InvalidTransferCount,
            ),
        ];

        for (input, slots, expected) in cases {
            let mut sequence = SequenceTracker::new();
            let error = decoder
                .decode(&input, slots, policy, &mut sequence)
                .unwrap_err();
            assert_eq!(error.code(), expected);
            assert_eq!(sequence.last_accepted(), None);
        }
    }

    #[test]
    fn generated_policy_can_exceed_a_stricter_caller_limit() {
        let limits = limits(8, 1);
        let policy = policy(7, 0, 16, 0, 2);
        let bytes = frame(PROTOCOL_MAJOR, PROTOCOL_MINOR, 7, 0, 0, 1, b"");

        DesktopFrameDecoder::current(limits)
            .decode(&bytes, 0, policy, &mut SequenceTracker::new())
            .unwrap();
    }

    #[test]
    fn direction_local_sequence_allows_gaps_but_not_duplicates_or_regressions() {
        let limits = limits(8, 1);
        let decoder = DesktopFrameDecoder::current(limits);
        let policy = policy(7, 0, 4, 0, 0);
        let mut host_to_engine = SequenceTracker::new();
        let mut engine_to_host = SequenceTracker::new();
        for sequence in [1, 8, u64::MAX] {
            let bytes = frame(PROTOCOL_MAJOR, PROTOCOL_MINOR, 7, 0, 0, sequence, b"");
            decoder
                .decode(&bytes, 0, policy, &mut host_to_engine)
                .unwrap();
        }
        let independent = frame(PROTOCOL_MAJOR, PROTOCOL_MINOR, 7, 0, 0, 1, b"");
        decoder
            .decode(&independent, 0, policy, &mut engine_to_host)
            .unwrap();

        for rejected in [u64::MAX, 8, 1, 0] {
            let bytes = frame(PROTOCOL_MAJOR, PROTOCOL_MINOR, 7, 0, 0, rejected, b"");
            let error = decoder
                .decode(&bytes, 0, policy, &mut host_to_engine)
                .unwrap_err();
            assert_eq!(error.code(), ProtocolErrorCode::NonMonotonicSequence);
            assert_eq!(host_to_engine.last_accepted(), Some(u64::MAX));
        }
        assert_eq!(engine_to_host.last_accepted(), Some(1));
    }

    #[test]
    fn debug_redacts_payload_bytes() {
        let limits = limits(8, 1);
        let decoder = DesktopFrameDecoder::current(limits);
        let policy = policy(7, 0, 8, 0, 0);
        let bytes = frame(PROTOCOL_MAJOR, PROTOCOL_MINOR, 7, 0, 6, 1, b"secret");
        let accepted = decoder
            .decode(&bytes, 0, policy, &mut SequenceTracker::new())
            .unwrap();
        let debug = format!("{accepted:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("secret"));
    }
}
