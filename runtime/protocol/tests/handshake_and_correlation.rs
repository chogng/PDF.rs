use pdf_rs_protocol::{
    COMMAND_DESCRIPTORS, Correlation, DESKTOP_BYTE_ORDER, DESKTOP_FRAME_HEADER_BYTES,
    DesktopFrameDecoder, ENVELOPE_HEADER_BYTES, EVENT_DESCRIPTORS, EndpointCapabilities,
    EndpointRole, HandshakeCompatibility, KNOWN_ENDPOINT_CAPABILITIES, PROTOCOL_MAJOR,
    PROTOCOL_MINOR, ProtocolErrorCode, ProtocolHello, ProtocolLimits, ProtocolValidator, RequestId,
    SCHEMA_HASH, SequenceTracker, SessionId, WorkerId,
};

fn hello(
    endpoint_role: EndpointRole,
    supported: u64,
    mandatory: u64,
    minor: u16,
    schema_hash: [u8; 16],
) -> ProtocolHello {
    ProtocolHello {
        major: PROTOCOL_MAJOR,
        minor,
        schema_hash,
        endpoint_role,
        capabilities: EndpointCapabilities {
            supported,
            mandatory,
        },
        max_message_bytes: 1_048_576,
        max_transfer_slots: 8,
    }
}

fn message_id(name: &str) -> u16 {
    COMMAND_DESCRIPTORS
        .iter()
        .find(|descriptor| descriptor.name == name)
        .unwrap_or_else(|| panic!("generated command {name} exists"))
        .id
}

fn empty_frame(message_type: u16, sequence: u64) -> Vec<u8> {
    let mut output = Vec::with_capacity(DESKTOP_FRAME_HEADER_BYTES);
    output.extend_from_slice(&PROTOCOL_MAJOR.to_le_bytes());
    output.extend_from_slice(&PROTOCOL_MINOR.to_le_bytes());
    output.extend_from_slice(&message_type.to_le_bytes());
    output.extend_from_slice(&0_u16.to_le_bytes());
    output.extend_from_slice(&0_u32.to_le_bytes());
    output.extend_from_slice(&sequence.to_le_bytes());
    output
}

#[test]
fn frame_policy_and_fixed_header_are_derived_from_the_generated_registry() {
    assert_eq!(DESKTOP_FRAME_HEADER_BYTES, ENVELOPE_HEADER_BYTES);
    assert_eq!(DESKTOP_BYTE_ORDER, "little-endian");
    let limits = ProtocolLimits::default();
    let validator = ProtocolValidator::new(limits);
    for descriptor in COMMAND_DESCRIPTORS.iter().chain(EVENT_DESCRIPTORS) {
        let policy = validator.frame_policy(descriptor.id).unwrap();
        assert_eq!(policy.message_type(), descriptor.id);
        assert_eq!(policy.allowed_flags(), descriptor.allowed_flags);
        assert_eq!(policy.max_payload_bytes(), descriptor.max_payload_bytes);
        assert_eq!(policy.min_transfer_slots(), descriptor.min_transfer_slots);
        assert_eq!(policy.max_transfer_slots(), descriptor.max_transfer_slots);
    }
    assert_eq!(
        validator.frame_policy(u16::MAX).unwrap_err().code(),
        ProtocolErrorCode::UnknownMessage
    );

    let hello = message_id("Hello");
    let frame = empty_frame(hello, 1);
    let mut sequence = SequenceTracker::new();
    let accepted = DesktopFrameDecoder::current(limits)
        .decode(
            &frame,
            0,
            validator.frame_policy(hello).unwrap(),
            &mut sequence,
        )
        .unwrap();
    assert!(accepted.payload().is_empty());
}

#[test]
fn exact_and_compatible_minor_handshakes_negotiate_bounds_and_known_capabilities() {
    let validator = ProtocolValidator::new(ProtocolLimits::default());
    let known = KNOWN_ENDPOINT_CAPABILITIES;
    assert_ne!(
        known, 0,
        "generated endpoint capability registry is nonempty"
    );
    let local = hello(EndpointRole::Host, known, 0, PROTOCOL_MINOR, SCHEMA_HASH);
    let peer = hello(EndpointRole::Engine, known, 0, PROTOCOL_MINOR, SCHEMA_HASH);
    let exact = validator.validate_handshake(&local, &peer).unwrap();
    assert_eq!(exact.compatibility(), HandshakeCompatibility::ExactSchema);
    assert_eq!(exact.minor(), PROTOCOL_MINOR);
    assert_eq!(exact.capabilities(), known);
    assert_eq!(exact.max_message_bytes(), 1_048_576);
    assert_eq!(exact.max_transfer_slots(), 8);

    let unknown_capabilities = !known;
    assert_ne!(unknown_capabilities, 0);
    let unknown_optional = 1_u64 << unknown_capabilities.trailing_zeros();
    let peer_with_unknown_optional = hello(
        EndpointRole::Engine,
        known | unknown_optional,
        0,
        PROTOCOL_MINOR,
        SCHEMA_HASH,
    );
    let accepted = validator
        .validate_handshake(&local, &peer_with_unknown_optional)
        .unwrap();
    assert_eq!(
        accepted.compatibility(),
        HandshakeCompatibility::ExactSchema
    );
    assert_eq!(accepted.capabilities(), known);

    if PROTOCOL_MINOR > 0 {
        let mut older_hash = SCHEMA_HASH;
        older_hash[0] ^= 0xff;
        let older = hello(
            EndpointRole::Engine,
            known,
            0,
            PROTOCOL_MINOR - 1,
            older_hash,
        );
        let compatible = validator.validate_handshake(&local, &older).unwrap();
        assert_eq!(
            compatible.compatibility(),
            HandshakeCompatibility::CompatibleMinor
        );
        assert_eq!(compatible.minor(), PROTOCOL_MINOR - 1);

        let new_engine = hello(EndpointRole::Engine, known, 0, PROTOCOL_MINOR, SCHEMA_HASH);
        let old_host = hello(EndpointRole::Host, known, 0, PROTOCOL_MINOR - 1, older_hash);
        let reverse = validator
            .validate_handshake(&new_engine, &old_host)
            .unwrap();
        assert_eq!(
            reverse.compatibility(),
            HandshakeCompatibility::CompatibleMinor
        );
        assert_eq!(reverse.minor(), PROTOCOL_MINOR - 1);
    }
}

#[test]
fn unknown_and_missing_mandatory_capabilities_have_distinct_stable_failures() {
    let validator = ProtocolValidator::new(ProtocolLimits::default());
    let local = hello(
        EndpointRole::Host,
        KNOWN_ENDPOINT_CAPABILITIES,
        0,
        PROTOCOL_MINOR,
        SCHEMA_HASH,
    );
    let unknown_capabilities = !KNOWN_ENDPOINT_CAPABILITIES;
    assert_ne!(unknown_capabilities, 0);
    let unknown_bit = 1_u64 << unknown_capabilities.trailing_zeros();
    let unknown = hello(
        EndpointRole::Engine,
        KNOWN_ENDPOINT_CAPABILITIES,
        unknown_bit,
        PROTOCOL_MINOR,
        SCHEMA_HASH,
    );
    let error = validator.validate_handshake(&local, &unknown).unwrap_err();
    assert_eq!(error.code(), ProtocolErrorCode::UnknownMandatoryCapability);
    assert_eq!(error.diagnostic_id(), "RPE-PROTOCOL-0025");

    let one_known = KNOWN_ENDPOINT_CAPABILITIES & KNOWN_ENDPOINT_CAPABILITIES.wrapping_neg();
    assert_ne!(one_known, 0);
    let missing = hello(EndpointRole::Engine, 0, 0, PROTOCOL_MINOR, SCHEMA_HASH);
    let requiring_local = hello(
        EndpointRole::Host,
        KNOWN_ENDPOINT_CAPABILITIES,
        one_known,
        PROTOCOL_MINOR,
        SCHEMA_HASH,
    );
    let error = validator
        .validate_handshake(&requiring_local, &missing)
        .unwrap_err();
    assert_eq!(error.code(), ProtocolErrorCode::MissingMandatoryCapability);
    assert_eq!(error.diagnostic_id(), "RPE-PROTOCOL-0026");
}

#[test]
fn same_minor_schema_fork_role_version_and_endpoint_limits_fail_closed() {
    let validator = ProtocolValidator::new(ProtocolLimits::default());
    let local = hello(
        EndpointRole::Host,
        KNOWN_ENDPOINT_CAPABILITIES,
        0,
        PROTOCOL_MINOR,
        SCHEMA_HASH,
    );
    let mut fork_hash = SCHEMA_HASH;
    fork_hash[15] ^= 1;
    let cases = [
        (
            hello(
                EndpointRole::Engine,
                KNOWN_ENDPOINT_CAPABILITIES,
                0,
                PROTOCOL_MINOR,
                fork_hash,
            ),
            ProtocolErrorCode::IncompatibleSchema,
        ),
        (
            hello(
                EndpointRole::Host,
                KNOWN_ENDPOINT_CAPABILITIES,
                0,
                PROTOCOL_MINOR,
                SCHEMA_HASH,
            ),
            ProtocolErrorCode::InvalidEndpointRole,
        ),
        (
            {
                let mut value = hello(
                    EndpointRole::Engine,
                    KNOWN_ENDPOINT_CAPABILITIES,
                    0,
                    PROTOCOL_MINOR,
                    SCHEMA_HASH,
                );
                value.major = PROTOCOL_MAJOR + 1;
                value
            },
            ProtocolErrorCode::UnsupportedMajor,
        ),
        (
            {
                let mut value = hello(
                    EndpointRole::Engine,
                    KNOWN_ENDPOINT_CAPABILITIES,
                    0,
                    PROTOCOL_MINOR,
                    SCHEMA_HASH,
                );
                value.max_message_bytes = 0;
                value
            },
            ProtocolErrorCode::InvalidEndpointLimits,
        ),
    ];
    for (peer, expected) in cases {
        assert_eq!(
            validator
                .validate_handshake(&local, &peer)
                .unwrap_err()
                .code(),
            expected
        );
    }
}

#[test]
fn generated_correlation_shapes_bind_worker_session_request_and_generation() {
    let validator = ProtocolValidator::new(ProtocolLimits::default());
    let worker = WorkerId::new(7);
    let session = SessionId::new(11);
    let request = RequestId::new(13);
    let worker_only = Correlation {
        worker,
        session: None,
        request: None,
        generation: None,
    };
    validator
        .validate_correlation(message_id("Hello"), &worker_only, worker, None)
        .unwrap();

    let session_only = Correlation {
        worker,
        session: Some(session),
        request: None,
        generation: None,
    };
    validator
        .validate_correlation(
            message_id("CloseSession"),
            &session_only,
            worker,
            Some(session),
        )
        .unwrap();

    let request_shape = Correlation {
        worker,
        session: Some(session),
        request: Some(request),
        generation: None,
    };
    validator
        .validate_correlation(message_id("Cancel"), &request_shape, worker, Some(session))
        .unwrap();

    let generation_shape = Correlation {
        worker,
        session: Some(session),
        request: None,
        generation: Some(17),
    };
    validator
        .validate_correlation(
            message_id("SetViewport"),
            &generation_shape,
            worker,
            Some(session),
        )
        .unwrap();
}

#[test]
fn wrong_or_zero_correlation_never_passes_a_generated_shape() {
    let validator = ProtocolValidator::new(ProtocolLimits::default());
    let worker = WorkerId::new(7);
    let session = SessionId::new(11);
    let request = RequestId::new(13);
    let cases = [
        Correlation {
            worker: WorkerId::new(8),
            session: Some(session),
            request: None,
            generation: Some(1),
        },
        Correlation {
            worker,
            session: None,
            request: None,
            generation: Some(1),
        },
        Correlation {
            worker,
            session: Some(session),
            request: Some(request),
            generation: Some(1),
        },
        Correlation {
            worker,
            session: Some(session),
            request: None,
            generation: Some(0),
        },
    ];
    for invalid in cases {
        let error = validator
            .validate_correlation(message_id("SetViewport"), &invalid, worker, Some(session))
            .unwrap_err();
        assert_eq!(error.code(), ProtocolErrorCode::InvalidCorrelation);
    }
    let error = validator
        .validate_correlation(
            u16::MAX,
            &Correlation {
                worker,
                session: None,
                request: None,
                generation: None,
            },
            worker,
            None,
        )
        .unwrap_err();
    assert_eq!(error.code(), ProtocolErrorCode::UnknownMessage);
}
