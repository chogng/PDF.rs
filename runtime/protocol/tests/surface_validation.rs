use pdf_rs_protocol::{
    AlphaMode, CapabilityDecisionHash, CompatibleHandshake, ENDPOINT_CAPABILITY_LOCAL_MEMORY,
    ENDPOINT_CAPABILITY_SHARED_ARRAY_BUFFER, ENDPOINT_CAPABILITY_SHARED_MEMORY,
    ENDPOINT_CAPABILITY_TRANSFERABLE_ARRAY_BUFFER, ENDPOINT_CAPABILITY_TRANSFERABLE_IMAGE_BITMAP,
    EndpointCapabilities, EndpointRole, MemoryEpoch, NativeBackend, PROTOCOL_MAJOR, PROTOCOL_MINOR,
    PixelFormat, ProtocolErrorCode, ProtocolHello, ProtocolLimits, ProtocolValidator,
    RenderConfigHash, RenderPlanHash, RenderPlanId, RendererEpoch, SCHEMA_HASH, SceneHash,
    SessionId, SurfaceCoordinateSpace, SurfaceId, SurfaceMetadata, SurfaceOwner,
    SurfacePlanBinding, SurfaceRegion, SurfaceRenderIdentity, SurfaceTransport,
    SurfaceValidationContext, WorkerId,
};

const WORKER: WorkerId = WorkerId::new(7);
const SESSION: SessionId = SessionId::new(11);
const GENERATION: u64 = 13;
const REGION_BYTES: u64 = 64;
const LAYOUT_BYTES: u64 = 48;
const LEASE_TOKEN: u64 = 31;

fn validator() -> ProtocolValidator {
    ProtocolValidator::new(ProtocolLimits::default())
}

fn handshake(capabilities: u64) -> CompatibleHandshake {
    let hello = |endpoint_role| ProtocolHello {
        major: PROTOCOL_MAJOR,
        minor: PROTOCOL_MINOR,
        schema_hash: SCHEMA_HASH,
        endpoint_role,
        capabilities: EndpointCapabilities {
            supported: capabilities,
            mandatory: 0,
        },
        max_message_bytes: 1_048_576,
        max_transfer_slots: 8,
    };
    validator()
        .validate_handshake(&hello(EndpointRole::Host), &hello(EndpointRole::Engine))
        .unwrap()
}

fn region() -> SurfaceRegion {
    SurfaceRegion {
        page_index: 2,
        x: -17,
        y: 23,
        width: 101,
        height: 103,
        coordinate_space: SurfaceCoordinateSpace::DevicePixelsTopLeft,
    }
}

fn render_identity() -> SurfaceRenderIdentity {
    SurfaceRenderIdentity::new(
        RenderConfigHash::new([1; 32]),
        RendererEpoch::new(17),
        RenderPlanId::new(19),
        RenderPlanHash::new([2; 32]),
        SceneHash::new([3; 32]),
        CapabilityDecisionHash::new([4; 32]),
        NativeBackend::ReferenceCpu,
    )
}

fn plan() -> SurfacePlanBinding {
    SurfacePlanBinding::new(region(), render_identity())
}

fn metadata() -> SurfaceMetadata {
    let render = render_identity();
    SurfaceMetadata {
        id: SurfaceId::new(29),
        lease_token: LEASE_TOKEN,
        owner: SurfaceOwner {
            worker: WORKER,
            session: SESSION,
        },
        generation: GENERATION,
        region: region(),
        width: 4,
        height: 3,
        stride: 16,
        format: PixelFormat::Rgba8,
        alpha: AlphaMode::Straight,
        byte_offset: 8,
        byte_length: LAYOUT_BYTES,
        render_config: render.render_config(),
        renderer_epoch: render.renderer_epoch(),
        plan_id: render.plan_id(),
        plan_hash: render.plan_hash(),
        scene_hash: render.scene_hash(),
        decision_hash: render.decision_hash(),
        backend: render.backend(),
    }
}

fn context(capabilities: u64, transfer_slots: usize) -> SurfaceValidationContext {
    SurfaceValidationContext::new(
        WORKER,
        SESSION,
        GENERATION,
        plan(),
        handshake(capabilities),
        transfer_slots,
    )
}

#[test]
fn every_surface_transport_binds_exact_negotiated_receiver_resource() {
    let base = metadata();
    let mut bitmap_metadata = base.clone();
    bitmap_metadata.alpha = AlphaMode::Premultiplied;
    bitmap_metadata.byte_offset = 0;

    let cases =
        vec![
            (
                base.clone(),
                SurfaceTransport::BrowserArrayBuffer {
                    slot: 0,
                    buffer_length: REGION_BYTES,
                },
                context(ENDPOINT_CAPABILITY_TRANSFERABLE_ARRAY_BUFFER, 1)
                    .with_browser_array_buffer(0, REGION_BYTES, true),
            ),
            (
                bitmap_metadata,
                SurfaceTransport::BrowserImageBitmap {
                    slot: 0,
                    width: 4,
                    height: 3,
                },
                context(ENDPOINT_CAPABILITY_TRANSFERABLE_IMAGE_BITMAP, 1)
                    .with_browser_image_bitmap(0, 4, 3, true),
            ),
            (
                base.clone(),
                SurfaceTransport::BrowserSharedArrayBuffer {
                    attachment_slot: 0,
                    buffer_length: REGION_BYTES,
                    fence_byte_offset: 0,
                    publication_epoch: 41,
                },
                context(ENDPOINT_CAPABILITY_SHARED_ARRAY_BUFFER, 1)
                    .with_browser_shared_array_buffer(0, REGION_BYTES, true, 0, 41),
            ),
            (
                base.clone(),
                SurfaceTransport::SharedMemory {
                    slot: 0,
                    region_length: REGION_BYTES,
                },
                context(ENDPOINT_CAPABILITY_SHARED_MEMORY, 1).with_shared_memory(0, REGION_BYTES),
            ),
            (
                base,
                SurfaceTransport::LocalMemory {
                    region_length: REGION_BYTES,
                    memory_epoch: MemoryEpoch::new(43),
                },
                context(ENDPOINT_CAPABILITY_LOCAL_MEMORY, 0)
                    .with_local_memory(MemoryEpoch::new(43), REGION_BYTES),
            ),
        ];

    for (metadata, transport, context) in cases {
        let surface = validator()
            .validate_surface(&metadata, &transport, &context)
            .unwrap();
        assert_eq!(surface.layout_bytes(), LAYOUT_BYTES);
        assert_eq!(surface.metadata(), &metadata);
        assert_eq!(surface.transport(), &transport);
    }
}

#[test]
fn capability_slot_kind_and_actual_resource_mismatches_fail_closed() {
    let metadata = metadata();
    let transport = SurfaceTransport::BrowserArrayBuffer {
        slot: 0,
        buffer_length: REGION_BYTES,
    };
    let cases =
        [
            (
                context(ENDPOINT_CAPABILITY_SHARED_MEMORY, 1).with_browser_array_buffer(
                    0,
                    REGION_BYTES,
                    true,
                ),
                ProtocolErrorCode::MissingEndpointCapability,
            ),
            (
                context(ENDPOINT_CAPABILITY_TRANSFERABLE_ARRAY_BUFFER, 0)
                    .with_browser_array_buffer(0, REGION_BYTES, true),
                ProtocolErrorCode::InvalidSurfaceSlot,
            ),
            (
                context(ENDPOINT_CAPABILITY_TRANSFERABLE_ARRAY_BUFFER, 1)
                    .with_browser_array_buffer(1, REGION_BYTES, true),
                ProtocolErrorCode::InvalidSurfaceSlot,
            ),
            (
                context(ENDPOINT_CAPABILITY_TRANSFERABLE_ARRAY_BUFFER, 1)
                    .with_browser_array_buffer(0, REGION_BYTES, false),
                ProtocolErrorCode::InvalidSurfaceSlot,
            ),
        ];
    for (context, expected) in cases {
        assert_eq!(
            validator()
                .validate_surface(&metadata, &transport, &context)
                .unwrap_err()
                .code(),
            expected
        );
    }
}

#[test]
fn owner_plan_epoch_lease_layout_and_range_mutations_are_rejected() {
    let transport = SurfaceTransport::SharedMemory {
        slot: 0,
        region_length: REGION_BYTES,
    };
    let context = context(ENDPOINT_CAPABILITY_SHARED_MEMORY, 1).with_shared_memory(0, REGION_BYTES);
    let original = metadata();
    let mut cases = Vec::new();

    let mut value = original.clone();
    value.lease_token = 0;
    cases.push((value, ProtocolErrorCode::InvalidSurfaceLease));
    let mut value = original.clone();
    value.owner.worker = WorkerId::new(8);
    cases.push((value, ProtocolErrorCode::InvalidSurfaceOwner));
    let mut value = original.clone();
    value.generation += 1;
    cases.push((value, ProtocolErrorCode::InvalidSurfaceEpoch));
    let mut value = original.clone();
    value.plan_hash = RenderPlanHash::new([9; 32]);
    cases.push((value, ProtocolErrorCode::InvalidSurfacePlan));
    let mut value = original.clone();
    value.region.x += 1;
    cases.push((value, ProtocolErrorCode::InvalidSurfaceRegion));
    let mut value = original.clone();
    value.stride = 15;
    cases.push((value, ProtocolErrorCode::InvalidSurfaceLayout));
    let mut value = original.clone();
    value.byte_length -= 1;
    cases.push((value, ProtocolErrorCode::InvalidSurfaceRange));
    let mut value = original;
    value.byte_offset = u64::MAX;
    cases.push((value, ProtocolErrorCode::NumericOverflow));

    for (mutated, expected) in cases {
        assert_eq!(
            validator()
                .validate_surface(&mutated, &transport, &context)
                .unwrap_err()
                .code(),
            expected
        );
    }
}

#[test]
fn image_bitmap_and_shared_fence_invariants_are_exact() {
    let mut bitmap_metadata = metadata();
    bitmap_metadata.alpha = AlphaMode::Premultiplied;
    bitmap_metadata.byte_offset = 0;
    let bitmap = SurfaceTransport::BrowserImageBitmap {
        slot: 0,
        width: 4,
        height: 3,
    };
    for context in [
        context(ENDPOINT_CAPABILITY_TRANSFERABLE_IMAGE_BITMAP, 1)
            .with_browser_image_bitmap(0, 5, 3, true),
        context(ENDPOINT_CAPABILITY_TRANSFERABLE_IMAGE_BITMAP, 1)
            .with_browser_image_bitmap(0, 4, 3, false),
    ] {
        assert_eq!(
            validator()
                .validate_surface(&bitmap_metadata, &bitmap, &context)
                .unwrap_err()
                .code(),
            ProtocolErrorCode::InvalidSurfaceSlot
        );
    }

    let shared = SurfaceTransport::BrowserSharedArrayBuffer {
        attachment_slot: 0,
        buffer_length: REGION_BYTES,
        fence_byte_offset: 8,
        publication_epoch: 41,
    };
    let shared_context = context(ENDPOINT_CAPABILITY_SHARED_ARRAY_BUFFER, 1)
        .with_browser_shared_array_buffer(0, REGION_BYTES, true, 8, 41);
    assert_eq!(
        validator()
            .validate_surface(&metadata(), &shared, &shared_context)
            .unwrap_err()
            .code(),
        ProtocolErrorCode::InvalidSharedFence
    );

    let stale_epoch = SurfaceTransport::BrowserSharedArrayBuffer {
        attachment_slot: 0,
        buffer_length: REGION_BYTES,
        fence_byte_offset: 0,
        publication_epoch: 42,
    };
    assert_eq!(
        validator()
            .validate_surface(
                &metadata(),
                &stale_epoch,
                &context(ENDPOINT_CAPABILITY_SHARED_ARRAY_BUFFER, 1)
                    .with_browser_shared_array_buffer(0, REGION_BYTES, true, 0, 41),
            )
            .unwrap_err()
            .code(),
        ProtocolErrorCode::InvalidSharedFence
    );
}

#[test]
fn debug_output_redacts_lease_and_surface_payloads() {
    let metadata = metadata();
    let debug = format!("{metadata:?}");
    assert!(debug.contains("[REDACTED]"));
    assert!(!debug.contains(&LEASE_TOKEN.to_string()));

    let context = context(ENDPOINT_CAPABILITY_SHARED_MEMORY, 1).with_shared_memory(0, REGION_BYTES);
    let surface = validator()
        .validate_surface(
            &metadata,
            &SurfaceTransport::SharedMemory {
                slot: 0,
                region_length: REGION_BYTES,
            },
            &context,
        )
        .unwrap();
    assert!(format!("{surface:?}").contains("[REDACTED]"));
}
