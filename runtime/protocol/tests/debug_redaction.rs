use pdf_rs_protocol::{
    ByteRange, CapabilityDecisionHash, DataAttachmentRole, DataSegment, DataTicket,
    EndpointCapabilities, EndpointRole, NativeBackend, PROTOCOL_MAJOR, PROTOCOL_MINOR,
    ProtocolHello, ProtocolLimits, ProtocolValidator, ProvideDataCommand, ReleaseSurfaceCommand,
    RenderConfigHash, RenderPlanHash, RenderPlanId, RendererEpoch, SCHEMA_HASH, SceneHash,
    SessionId, SourceIdentity, SurfaceCoordinateSpace, SurfaceId, SurfacePlanBinding,
    SurfaceRegion, SurfaceRenderIdentity, SurfaceValidationContext, WorkerId,
};

#[test]
fn generated_debug_redacts_sensitive_lease_fields() {
    let command = ReleaseSurfaceCommand {
        surface: SurfaceId::new(3_735_928_559),
        lease_token: 4_026_531_841,
    };
    let debug = format!("{command:?}");
    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains("4026531841"));
}

#[test]
fn handwritten_surface_validation_wrappers_preserve_schema_privacy() {
    let scene_hash = SceneHash::new([0x5a; 32]);
    let decision_hash = CapabilityDecisionHash::new([0x6b; 32]);
    let render = SurfaceRenderIdentity::new(
        RenderConfigHash::new([0x11; 32]),
        RendererEpoch::new(3),
        RenderPlanId::new(5),
        RenderPlanHash::new([0x22; 32]),
        scene_hash,
        decision_hash,
        NativeBackend::ReferenceCpu,
    );
    let plan = SurfacePlanBinding::new(
        SurfaceRegion {
            page_index: 0,
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            coordinate_space: SurfaceCoordinateSpace::DevicePixelsTopLeft,
        },
        render,
    );
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
    let handshake = ProtocolValidator::new(ProtocolLimits::default())
        .validate_handshake(&hello(EndpointRole::Host), &hello(EndpointRole::Engine))
        .unwrap();
    let context =
        SurfaceValidationContext::new(WorkerId::new(7), SessionId::new(11), 13, plan, handshake, 0);

    for debug in [format!("{render:?}"), format!("{context:?}")] {
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains(&format!("{scene_hash:?}")));
        assert!(!debug.contains(&format!("{decision_hash:?}")));
    }
}

#[test]
fn generated_debug_recursively_redacts_private_and_sensitive_record_fields() {
    let source = SourceIdentity {
        stable_id: [0xab; 32],
        revision: 7,
    };
    let command = ProvideDataCommand {
        ticket: DataTicket::new(11),
        source,
        segments: vec![DataSegment {
            range: ByteRange { start: 13, len: 17 },
            slot: 0,
            byte_length: 17,
            role: DataAttachmentRole::ImmutableRangeBytes,
        }],
    };
    let debug = format!("{command:?}");

    assert!(debug.contains("ticket"));
    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains("171"));
    assert!(!debug.contains("start: 13"));
    assert!(!debug.contains("byte_length: 17"));
}
