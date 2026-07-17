use std::fmt;

use crate::{
    CommandEnvelope, CompatibleHandshake, EnvelopeHeader, EventEnvelope, MAX_MESSAGE_BYTES,
    MAX_TRANSFER_SLOTS, MESSAGE_ID_ENGINE_HELLO, MESSAGE_ID_HELLO, MESSAGE_ID_HELLO_ACCEPT,
    MESSAGE_ID_PROTOCOL_FAULT, MESSAGE_ID_READY, MessageDescriptor, MessageKind, PROTOCOL_MAJOR,
    PROTOCOL_MINOR, PayloadCodecLimits, ProtocolError, ProtocolErrorCode, ProtocolLimits,
    decode_command_payload, decode_event_payload, descriptor_by_id,
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
    kind: MessageKind,
    message_type: u16,
    allowed_flags: u16,
    max_payload_bytes: u32,
    maximum_encoded_payload_bytes: u32,
    min_transfer_slots: u16,
    max_transfer_slots: u16,
    required_capability: u64,
}

impl FrameMessagePolicy {
    pub(crate) fn from_descriptor(descriptor: &MessageDescriptor) -> Result<Self, ProtocolError> {
        if descriptor.max_payload_bytes == 0
            || descriptor.max_payload_bytes > MAX_MESSAGE_BYTES
            || descriptor.maximum_encoded_payload_bytes == 0
            || descriptor.maximum_encoded_payload_bytes > descriptor.max_payload_bytes
            || descriptor.min_transfer_slots > descriptor.max_transfer_slots
            || descriptor.max_transfer_slots > MAX_TRANSFER_SLOTS
        {
            return Err(ProtocolError::for_code(
                ProtocolErrorCode::InvalidGeneratedDescriptor,
            ));
        }
        Ok(Self {
            kind: descriptor.kind,
            message_type: descriptor.id,
            allowed_flags: descriptor.allowed_flags,
            max_payload_bytes: descriptor.max_payload_bytes,
            maximum_encoded_payload_bytes: descriptor.maximum_encoded_payload_bytes,
            min_transfer_slots: descriptor.min_transfer_slots,
            max_transfer_slots: descriptor.max_transfer_slots,
            required_capability: descriptor.required_capability,
        })
    }

    /// Returns whether the generated descriptor denotes a command or event.
    pub const fn kind(self) -> MessageKind {
        self.kind
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

    /// Returns the proved maximum bytes of `Correlation || record` for this exact schema.
    pub const fn maximum_encoded_payload_bytes(self) -> u32 {
        self.maximum_encoded_payload_bytes
    }

    /// Returns the minimum required out-of-band transfer count.
    pub const fn min_transfer_slots(self) -> u16 {
        self.min_transfer_slots
    }

    /// Returns the maximum accepted out-of-band transfer count.
    pub const fn max_transfer_slots(self) -> u16 {
        self.max_transfer_slots
    }

    /// Returns the endpoint capability required by this message, or zero.
    pub const fn required_capability(self) -> u64 {
        self.required_capability
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

fn generated_policy(message_type: u16) -> Result<FrameMessagePolicy, ProtocolError> {
    let descriptor = descriptor_by_id(message_type)
        .ok_or_else(|| ProtocolError::for_code(ProtocolErrorCode::UnknownMessage))?;
    FrameMessagePolicy::from_descriptor(descriptor)
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

/// Prepared frame whose sequence has not yet been committed.
///
/// Callers decode the generated payload, validate correlation, state, and out-of-band resources,
/// and only then consume this value with [`PendingDesktopFrame::commit`].
#[derive(Clone, Eq, PartialEq)]
pub struct PendingDesktopFrame<'frame> {
    header: EnvelopeHeader,
    policy: FrameMessagePolicy,
    transfer_slots: u16,
    payload: &'frame [u8],
    expected_last_sequence: Option<u64>,
}

impl<'frame> PendingDesktopFrame<'frame> {
    /// Returns the validated fixed header.
    pub const fn header(&self) -> &EnvelopeHeader {
        &self.header
    }

    /// Returns the generated registry policy selected from the header message identifier.
    pub const fn policy(&self) -> FrameMessagePolicy {
        self.policy
    }

    /// Returns the observed logical out-of-band resource count.
    pub const fn transfer_slots(&self) -> u16 {
        self.transfer_slots
    }

    /// Borrows the exact length-checked canonical payload.
    pub const fn payload(&self) -> &'frame [u8] {
        self.payload
    }

    /// Decodes a command with the generated exact `fixed_le_v1` codec.
    pub fn decode_command(&self) -> Result<CommandEnvelope, ProtocolError> {
        if self.policy.kind != MessageKind::Command {
            return Err(ProtocolError::for_code(
                ProtocolErrorCode::InvalidMessageBinding,
            ));
        }
        decode_command_payload(
            self.header.clone(),
            self.payload,
            PayloadCodecLimits::protocol_default(),
        )
        .map_err(|_| ProtocolError::for_code(ProtocolErrorCode::InvalidPayloadEncoding))
    }

    /// Decodes an event with the generated exact `fixed_le_v1` codec.
    pub fn decode_event(&self) -> Result<EventEnvelope, ProtocolError> {
        if self.policy.kind != MessageKind::Event {
            return Err(ProtocolError::for_code(
                ProtocolErrorCode::InvalidMessageBinding,
            ));
        }
        decode_event_payload(
            self.header.clone(),
            self.payload,
            PayloadCodecLimits::protocol_default(),
        )
        .map_err(|_| ProtocolError::for_code(ProtocolErrorCode::InvalidPayloadEncoding))
    }

    /// Commits the direction-local sequence after all higher-level validation succeeds.
    pub fn commit(
        self,
        sequence: &mut SequenceTracker,
    ) -> Result<ValidatedDesktopFrame<'frame>, ProtocolError> {
        if sequence.last_accepted != self.expected_last_sequence {
            return Err(ProtocolError::for_code(
                ProtocolErrorCode::NonMonotonicSequence,
            ));
        }
        sequence.validate(self.header.sequence)?;
        sequence.commit(self.header.sequence);
        Ok(ValidatedDesktopFrame {
            header: self.header,
            policy: self.policy,
            transfer_slots: self.transfer_slots,
            payload: self.payload,
        })
    }
}

impl fmt::Debug for PendingDesktopFrame<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PendingDesktopFrame")
            .field("header", &self.header)
            .field("policy", &self.policy)
            .field("transfer_slots", &self.transfer_slots)
            .field("payload_len", &self.payload.len())
            .field("payload", &"[REDACTED]")
            .finish()
    }
}

/// Exact-schema frame validator used only while establishing a handshake.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HandshakeFrameDecoder {
    limits: ProtocolLimits,
}

impl HandshakeFrameDecoder {
    /// Creates a bootstrap decoder for the one compiled `(major, minor, codec ABI)` registry.
    pub const fn new(limits: ProtocolLimits) -> Self {
        Self { limits }
    }

    /// Prepares one exact-schema handshake frame without committing its sequence.
    pub fn prepare<'frame>(
        &self,
        frame: &'frame [u8],
        transfer_slots: usize,
        sequence: &SequenceTracker,
    ) -> Result<PendingDesktopFrame<'frame>, ProtocolError> {
        prepare_exact_frame(
            frame,
            transfer_slots,
            sequence,
            FrameDecodeContext {
                limits: self.limits,
                max_payload_bytes: self.limits.max_payload_bytes(),
                max_transfer_slots: self.limits.max_transfer_slots(),
                negotiated_capabilities: 0,
                phase: FramePhase::Handshake,
            },
        )
    }
}

/// Stateless desktop frame validator bound to one successful exact-schema handshake.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DesktopFrameDecoder {
    limits: ProtocolLimits,
    negotiated_capabilities: u64,
    max_payload_bytes: u32,
    max_transfer_slots: u16,
}

impl DesktopFrameDecoder {
    /// Creates a decoder only from a successful exact-schema handshake.
    pub const fn for_handshake(handshake: CompatibleHandshake) -> Self {
        Self {
            limits: handshake.protocol_limits(),
            negotiated_capabilities: handshake.capabilities(),
            max_payload_bytes: handshake.max_message_bytes(),
            max_transfer_slots: handshake.max_transfer_slots(),
        }
    }

    /// Returns the exact negotiated protocol major.
    pub const fn major(self) -> u16 {
        PROTOCOL_MAJOR
    }

    /// Returns the exact negotiated protocol minor.
    pub const fn minor(self) -> u16 {
        PROTOCOL_MINOR
    }

    /// Returns the validated global limits.
    pub const fn limits(self) -> ProtocolLimits {
        self.limits
    }

    /// Prepares one post-handshake frame without committing its sequence.
    pub fn prepare<'frame>(
        &self,
        frame: &'frame [u8],
        transfer_slots: usize,
        sequence: &SequenceTracker,
    ) -> Result<PendingDesktopFrame<'frame>, ProtocolError> {
        prepare_exact_frame(
            frame,
            transfer_slots,
            sequence,
            FrameDecodeContext {
                limits: self.limits,
                max_payload_bytes: self.max_payload_bytes,
                max_transfer_slots: self.max_transfer_slots,
                negotiated_capabilities: self.negotiated_capabilities,
                phase: FramePhase::Negotiated,
            },
        )
    }
}

#[derive(Clone, Copy)]
enum FramePhase {
    Handshake,
    Negotiated,
}

impl FramePhase {
    const fn allows(self, message_type: u16) -> bool {
        match self {
            Self::Handshake => matches!(
                message_type,
                MESSAGE_ID_HELLO
                    | MESSAGE_ID_HELLO_ACCEPT
                    | MESSAGE_ID_ENGINE_HELLO
                    | MESSAGE_ID_READY
                    | MESSAGE_ID_PROTOCOL_FAULT
            ),
            Self::Negotiated => true,
        }
    }
}

#[derive(Clone, Copy)]
struct FrameDecodeContext {
    limits: ProtocolLimits,
    max_payload_bytes: u32,
    max_transfer_slots: u16,
    negotiated_capabilities: u64,
    phase: FramePhase,
}

fn prepare_exact_frame<'frame>(
    frame: &'frame [u8],
    transfer_slots: usize,
    sequence: &SequenceTracker,
    context: FrameDecodeContext,
) -> Result<PendingDesktopFrame<'frame>, ProtocolError> {
    if frame.len() < DESKTOP_FRAME_HEADER_BYTES {
        return Err(ProtocolError::for_code(ProtocolErrorCode::TruncatedHeader));
    }
    let frame_len = u64::try_from(frame.len())
        .map_err(|_| ProtocolError::for_code(ProtocolErrorCode::FrameTooLarge))?;
    if frame_len > context.limits.max_frame_bytes() {
        return Err(ProtocolError::for_code(ProtocolErrorCode::FrameTooLarge));
    }

    let header = decode_header(frame)?;
    if header.major != PROTOCOL_MAJOR {
        return Err(ProtocolError::for_code(ProtocolErrorCode::UnsupportedMajor));
    }
    if header.minor != PROTOCOL_MINOR {
        return Err(ProtocolError::for_code(ProtocolErrorCode::UnsupportedMinor));
    }
    if !context.phase.allows(header.message_type) {
        return Err(ProtocolError::for_code(ProtocolErrorCode::UnknownMessage));
    }
    let policy = generated_policy(header.message_type)?;
    if header.flags & !policy.allowed_flags != 0 {
        return Err(ProtocolError::for_code(ProtocolErrorCode::InvalidFlags));
    }
    if header.payload_len > context.limits.max_payload_bytes()
        || header.payload_len > context.max_payload_bytes
    {
        return Err(ProtocolError::for_code(ProtocolErrorCode::PayloadTooLarge));
    }
    if header.payload_len > policy.max_payload_bytes
        || header.payload_len > policy.maximum_encoded_payload_bytes
    {
        return Err(ProtocolError::for_code(
            ProtocolErrorCode::MessagePayloadTooLarge,
        ));
    }
    if policy.required_capability != 0
        && policy.required_capability & context.negotiated_capabilities == 0
    {
        return Err(ProtocolError::for_code(
            ProtocolErrorCode::MissingEndpointCapability,
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
    if transfer_slots > usize::from(context.limits.max_transfer_slots())
        || transfer_slots > usize::from(context.max_transfer_slots)
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
    Ok(PendingDesktopFrame {
        header,
        policy,
        transfer_slots,
        payload,
        expected_last_sequence: sequence.last_accepted,
    })
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
    use super::{DESKTOP_FRAME_HEADER_BYTES, DesktopFrameDecoder, SequenceTracker};
    use crate::{
        KNOWN_ENDPOINT_CAPABILITIES, MESSAGE_ID_CLOSE_SESSION, MESSAGE_ID_SHUTDOWN, PROTOCOL_MAJOR,
        PROTOCOL_MINOR, ProtocolErrorCode, ProtocolLimitConfig, ProtocolLimits,
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

    fn decoder(limits: ProtocolLimits) -> DesktopFrameDecoder {
        DesktopFrameDecoder {
            limits,
            negotiated_capabilities: KNOWN_ENDPOINT_CAPABILITIES,
            max_payload_bytes: limits.max_payload_bytes(),
            max_transfer_slots: limits.max_transfer_slots(),
        }
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
        let decoder = decoder(limits);
        let bytes = frame(
            PROTOCOL_MAJOR,
            PROTOCOL_MINOR,
            MESSAGE_ID_CLOSE_SESSION,
            0,
            3,
            41,
            b"abc",
        );
        let mut sequence = SequenceTracker::new();

        let pending = decoder.prepare(&bytes, 0, &sequence).unwrap();
        assert_eq!(sequence.last_accepted(), None);
        let accepted = pending.commit(&mut sequence).unwrap();

        assert_eq!(accepted.header().payload_len, 3);
        assert_eq!(accepted.payload(), b"abc");
        assert_eq!(accepted.transfer_slots(), 0);
        assert_eq!(sequence.last_accepted(), Some(41));
    }

    #[test]
    fn framing_version_message_flags_and_lengths_fail_before_sequence_commit() {
        let limits = limits(8, 2);
        let decoder = decoder(limits);
        let valid = frame(
            PROTOCOL_MAJOR,
            PROTOCOL_MINOR,
            MESSAGE_ID_CLOSE_SESSION,
            0,
            1,
            9,
            b"x",
        );
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
                frame(
                    PROTOCOL_MAJOR + 1,
                    PROTOCOL_MINOR,
                    MESSAGE_ID_CLOSE_SESSION,
                    0,
                    1,
                    9,
                    b"x",
                ),
                0,
                ProtocolErrorCode::UnsupportedMajor,
            ),
            (
                frame(
                    PROTOCOL_MAJOR,
                    PROTOCOL_MINOR + 1,
                    MESSAGE_ID_CLOSE_SESSION,
                    0,
                    1,
                    9,
                    b"x",
                ),
                0,
                ProtocolErrorCode::UnsupportedMinor,
            ),
            (
                frame(PROTOCOL_MAJOR, PROTOCOL_MINOR, u16::MAX, 0, 1, 9, b"x"),
                0,
                ProtocolErrorCode::UnknownMessage,
            ),
            (
                frame(
                    PROTOCOL_MAJOR,
                    PROTOCOL_MINOR,
                    MESSAGE_ID_CLOSE_SESSION,
                    2,
                    1,
                    9,
                    b"x",
                ),
                0,
                ProtocolErrorCode::InvalidFlags,
            ),
            (
                frame(
                    PROTOCOL_MAJOR,
                    PROTOCOL_MINOR,
                    MESSAGE_ID_CLOSE_SESSION,
                    0,
                    9,
                    9,
                    b"x",
                ),
                0,
                ProtocolErrorCode::PayloadTooLarge,
            ),
            (
                frame(
                    PROTOCOL_MAJOR,
                    PROTOCOL_MINOR,
                    MESSAGE_ID_CLOSE_SESSION,
                    0,
                    2,
                    9,
                    b"x",
                ),
                0,
                ProtocolErrorCode::FrameLengthMismatch,
            ),
            (valid.clone(), 1, ProtocolErrorCode::InvalidTransferCount),
            (
                valid.clone(),
                usize::MAX,
                ProtocolErrorCode::InvalidTransferCount,
            ),
        ];

        for (input, slots, expected) in cases {
            let sequence = SequenceTracker::new();
            let error = decoder.prepare(&input, slots, &sequence).unwrap_err();
            assert_eq!(error.code(), expected);
            assert_eq!(sequence.last_accepted(), None);
        }
    }

    #[test]
    fn generated_exact_payload_maximum_is_enforced() {
        let limits = limits(32, 1);
        let bytes = frame(
            PROTOCOL_MAJOR,
            PROTOCOL_MINOR,
            MESSAGE_ID_SHUTDOWN,
            0,
            16,
            1,
            &[0; 16],
        );
        let error = decoder(limits)
            .prepare(&bytes, 0, &SequenceTracker::new())
            .unwrap_err();
        assert_eq!(error.code(), ProtocolErrorCode::MessagePayloadTooLarge);
    }

    #[test]
    fn direction_local_sequence_allows_gaps_but_not_duplicates_or_regressions() {
        let limits = limits(8, 1);
        let decoder = decoder(limits);
        let mut host_to_engine = SequenceTracker::new();
        let mut engine_to_host = SequenceTracker::new();
        for sequence in [1, 8, u64::MAX] {
            let bytes = frame(
                PROTOCOL_MAJOR,
                PROTOCOL_MINOR,
                MESSAGE_ID_CLOSE_SESSION,
                0,
                0,
                sequence,
                b"",
            );
            decoder
                .prepare(&bytes, 0, &host_to_engine)
                .unwrap()
                .commit(&mut host_to_engine)
                .unwrap();
        }
        let independent = frame(
            PROTOCOL_MAJOR,
            PROTOCOL_MINOR,
            MESSAGE_ID_CLOSE_SESSION,
            0,
            0,
            1,
            b"",
        );
        decoder
            .prepare(&independent, 0, &engine_to_host)
            .unwrap()
            .commit(&mut engine_to_host)
            .unwrap();

        for rejected in [u64::MAX, 8, 1, 0] {
            let bytes = frame(
                PROTOCOL_MAJOR,
                PROTOCOL_MINOR,
                MESSAGE_ID_CLOSE_SESSION,
                0,
                0,
                rejected,
                b"",
            );
            let error = decoder.prepare(&bytes, 0, &host_to_engine).unwrap_err();
            assert_eq!(error.code(), ProtocolErrorCode::NonMonotonicSequence);
            assert_eq!(host_to_engine.last_accepted(), Some(u64::MAX));
        }
        assert_eq!(engine_to_host.last_accepted(), Some(1));
    }

    #[test]
    fn debug_redacts_payload_bytes() {
        let limits = limits(8, 1);
        let decoder = decoder(limits);
        let bytes = frame(
            PROTOCOL_MAJOR,
            PROTOCOL_MINOR,
            MESSAGE_ID_CLOSE_SESSION,
            0,
            6,
            1,
            b"secret",
        );
        let pending = decoder.prepare(&bytes, 0, &SequenceTracker::new()).unwrap();
        let debug = format!("{pending:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains("secret"));
    }
}
