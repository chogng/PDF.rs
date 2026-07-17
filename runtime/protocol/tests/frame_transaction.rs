use pdf_rs_protocol::{
    CloseSessionCommand, Command, CommandEnvelope, Correlation, DESKTOP_FRAME_HEADER_BYTES,
    DesktopFrameDecoder, EndpointCapabilities, EndpointRole, EnvelopeHeader, HandshakeFrameDecoder,
    MESSAGE_ID_CLOSE_SESSION, MESSAGE_ID_SET_VIEWPORT, PROTOCOL_MAJOR, PROTOCOL_MINOR,
    PayloadCodecLimits, ProtocolErrorCode, ProtocolHello, ProtocolLimits, ProtocolValidator,
    SCHEMA_HASH, SequenceTracker, SessionId, WorkerId, encode_command_payload,
};

const WORKER: WorkerId = WorkerId::new(7);
const SESSION: SessionId = SessionId::new(11);

fn handshake() -> pdf_rs_protocol::CompatibleHandshake {
    let hello = |endpoint_role| ProtocolHello {
        major: PROTOCOL_MAJOR,
        minor: PROTOCOL_MINOR,
        schema_hash: SCHEMA_HASH,
        endpoint_role,
        capabilities: EndpointCapabilities {
            supported: 0,
            mandatory: 0,
        },
        max_message_bytes: 1_048_576,
        max_transfer_slots: 8,
    };
    ProtocolValidator::new(ProtocolLimits::default())
        .validate_handshake(&hello(EndpointRole::Host), &hello(EndpointRole::Engine))
        .unwrap()
}

fn close_envelope(sequence: u64) -> CommandEnvelope {
    CommandEnvelope {
        header: EnvelopeHeader {
            major: PROTOCOL_MAJOR,
            minor: PROTOCOL_MINOR,
            message_type: MESSAGE_ID_CLOSE_SESSION,
            flags: 0,
            payload_len: 19,
            sequence,
        },
        correlation: Correlation {
            worker: WORKER,
            session: Some(SESSION),
            request: None,
            generation: None,
        },
        command: Command::CloseSession(CloseSessionCommand {}),
    }
}

fn encoded_frame(envelope: &CommandEnvelope) -> Vec<u8> {
    let (message_id, payload) =
        encode_command_payload(envelope, PayloadCodecLimits::protocol_default()).unwrap();
    assert_eq!(message_id, envelope.header.message_type);
    let mut frame = Vec::with_capacity(DESKTOP_FRAME_HEADER_BYTES + payload.len());
    frame.extend_from_slice(&envelope.header.major.to_le_bytes());
    frame.extend_from_slice(&envelope.header.minor.to_le_bytes());
    frame.extend_from_slice(&message_id.to_le_bytes());
    frame.extend_from_slice(&envelope.header.flags.to_le_bytes());
    frame.extend_from_slice(&envelope.header.payload_len.to_le_bytes());
    frame.extend_from_slice(&envelope.header.sequence.to_le_bytes());
    frame.extend_from_slice(&payload);
    frame
}

#[test]
fn generated_decode_and_business_validation_precede_explicit_sequence_commit() {
    let envelope = close_envelope(3);
    let frame = encoded_frame(&envelope);
    let decoder = DesktopFrameDecoder::for_handshake(handshake());
    let validator = ProtocolValidator::new(ProtocolLimits::default());
    let mut sequence = SequenceTracker::new();

    let pending = decoder.prepare(&frame, 0, &sequence).unwrap();
    assert_eq!(sequence.last_accepted(), None);
    let decoded = pending.decode_command().unwrap();
    assert_eq!(decoded, envelope);
    validator
        .validate_command_payload_correlation(&decoded, WORKER, Some(SESSION))
        .unwrap();
    let accepted = pending.commit(&mut sequence).unwrap();

    assert_eq!(accepted.payload(), &frame[DESKTOP_FRAME_HEADER_BYTES..]);
    assert_eq!(sequence.last_accepted(), Some(3));
}

#[test]
fn canonical_payload_failure_never_commits_sequence() {
    let mut frame = encoded_frame(&close_envelope(4));
    frame[DESKTOP_FRAME_HEADER_BYTES + 8] = 2;
    let decoder = DesktopFrameDecoder::for_handshake(handshake());
    let sequence = SequenceTracker::new();

    let pending = decoder.prepare(&frame, 0, &sequence).unwrap();
    assert_eq!(
        pending.decode_command().unwrap_err().code(),
        ProtocolErrorCode::InvalidPayloadEncoding
    );
    assert_eq!(sequence.last_accepted(), None);
}

#[test]
fn pending_commit_detects_intervening_sequence_state_change() {
    let decoder = DesktopFrameDecoder::for_handshake(handshake());
    let mut sequence = SequenceTracker::new();
    let later_frame = encoded_frame(&close_envelope(2));
    let earlier_frame = encoded_frame(&close_envelope(1));

    let later = decoder.prepare(&later_frame, 0, &sequence).unwrap();
    decoder
        .prepare(&earlier_frame, 0, &sequence)
        .unwrap()
        .commit(&mut sequence)
        .unwrap();
    assert_eq!(
        later.commit(&mut sequence).unwrap_err().code(),
        ProtocolErrorCode::NonMonotonicSequence
    );
    assert_eq!(sequence.last_accepted(), Some(1));
}

#[test]
fn bootstrap_decoder_accepts_only_current_registry_handshake_messages() {
    let mut frame = encoded_frame(&close_envelope(1));
    frame[4..6].copy_from_slice(&MESSAGE_ID_SET_VIEWPORT.to_le_bytes());
    let error = HandshakeFrameDecoder::new(ProtocolLimits::default())
        .prepare(&frame, 0, &SequenceTracker::new())
        .unwrap_err();
    assert_eq!(error.code(), ProtocolErrorCode::UnknownMessage);

    frame[0..2].copy_from_slice(&(PROTOCOL_MAJOR + 1).to_le_bytes());
    let error = HandshakeFrameDecoder::new(ProtocolLimits::default())
        .prepare(&frame, 0, &SequenceTracker::new())
        .unwrap_err();
    assert_eq!(error.code(), ProtocolErrorCode::UnsupportedMajor);
}
